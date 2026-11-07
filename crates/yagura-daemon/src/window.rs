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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tokio::sync::{
    Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard, Notify, broadcast, mpsc, oneshot, watch,
};
use yagura_os::program;
use yagura_protocol::{
    DefaultPalettes, DetectedStatus, INSTANCE_ENV, MCP_BRIDGE_ENV, OnNameTaken, Palette, RoomRef,
    STATUS_TOKEN_ENV, UnknownReason, WINDOW_ID_ENV, WINDOW_NAME_MAX_BYTES, WindowInfo,
    WindowNameError, WindowStatus, validate_window_name, validate_window_vendor,
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
use crate::detector::reading::ComposerReadout;
use crate::handoff::{ReadyHoldLatch, StartupEvidence};
use crate::holder_host::HolderHost;
use crate::inbox::{Inbox, is_session_id};
use crate::input::{
    ComposerLine, InputProfile, ProbeChars, SubmitAction, SubmitOutcome, SubmitPlan, SubmitStep,
    TakeBack, plan_submit_line,
};
use crate::profile::CredentialsMode;
use crate::profile_delivery::ProfileDelivery;
use crate::pty::{
    AttachGeneration, GuardedWrite, LostNotice, PtyResult, PtySession, TakeBackCall,
    TakeBackPrefix, TakeBackRegistration, TakeBackSpawn, WindowSpawn, lock_recover,
};
use crate::room::{mention_key, reserved_mention};
use crate::token::StatusToken;
use crate::waiter::StatusAt;
use crate::window_table::{
    OpeningRow, Reservation, WindowRow, WindowTable, WindowValues, credentials_kind,
    credentials_mode, launch_record, recorded_launch,
};
use crate::workspace::{self, WorkspaceKey, resolve_key_blocking};

mod status_gate;

use status_gate::{Publication, StatusGate};

/// Capacity of the fan-out channel carrying confirmed status changes to
/// subscribers. Status changes are debounced and therefore rare, so this is
/// generous enough that a lagging subscriber means a stalled connection
/// rather than a burst.
pub(crate) const STATUS_BROADCAST_CAPACITY: usize = 64;

/// A window's `StatusEvent` auth token, set once: when the window opens, or
/// for a window taken back once its holder returns it.
type TokenSlot = Arc<OnceLock<Arc<StatusToken>>>;

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
/// that what lives only as long as one daemon run (approval request ids, the
/// run a holder is told of) does not match another run's.
pub(crate) fn boot_nonce() -> String {
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

/// Number of base36 digits after the `w` of a window id.
const WINDOW_ID_DIGITS: usize = 8;

/// How many random ids one reservation round offers to
/// [`first_free_window_id`]; each id is taken only by a holder's files or a
/// window, so a round runs dry only on a broken random source.
const WINDOW_ID_CANDIDATES: usize = 8;

/// How many reservation rounds an open makes before it gives up, each ending
/// when the table already holds the id the round picked.
const WINDOW_ID_ROUNDS: usize = 8;

/// A fresh random window id: `w` and [`WINDOW_ID_DIGITS`] base36 digits. It
/// outlives the daemon run that drew it, so nothing of the run is in it.
fn random_window_id() -> Result<String, WindowOpenError> {
    let mut random = [0u8; 8];
    yagura_os::fill_random(&mut random)
        .map_err(|err| WindowOpenError::SpawnFailed(format!("cannot draw a window id: {err}")))?;
    Ok(format!(
        "w{}",
        to_base36(u64::from_le_bytes(random), WINDOW_ID_DIGITS)
    ))
}

/// The first of `candidates` that no holder's files in `holders_dir` name
/// (its record and, on Unix, its socket and the socket's lock) and that
/// `in_use` does not claim; `None` when every candidate is taken. An id
/// named by a holder's files may still be a living window whose daemon
/// is gone.
fn first_free_window_id(
    candidates: impl IntoIterator<Item = String>,
    holders_dir: &Path,
    in_use: impl Fn(&str) -> bool,
) -> Option<String> {
    candidates.into_iter().find(|id| {
        let named_by_files = ["json", "sock", "sock.lock"]
            .iter()
            .any(|extension| holders_dir.join(format!("{id}.{extension}")).exists());
        !named_by_files && !in_use(id)
    })
}

/// A window's values as its row is first written: no layout or session yet.
fn window_values(
    provenance: &WindowProvenance,
    name: &str,
    cwd: &str,
    vendor: Option<&String>,
    command: &[String],
    detection: Option<&String>,
) -> WindowValues {
    let launch = provenance.launch.as_ref();
    let task = provenance.task_origin.as_ref();
    WindowValues {
        name: name.to_owned(),
        cwd: cwd.to_owned(),
        vendor: vendor.cloned(),
        command: command.to_vec(),
        detection: detection.cloned(),
        profile: launch.map(|launch| launch.profile.clone()),
        model: launch.and_then(|launch| launch.model.clone()),
        effort: launch.and_then(|launch| launch.effort.clone()),
        layout_json: None,
        session_id: None,
        opener: provenance.opener.clone(),
        task_plan_id: task.map(|task| task.plan_id.clone()),
        task_id: task.map(|task| task.task_id.clone()),
        credentials: credentials_kind(provenance.credentials),
    }
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
#[derive(Debug, Clone, Default)]
pub struct OpenWindowRequest {
    /// Argument vector to spawn (`command[0]` is the executable). Must be
    /// non-empty.
    pub command: Vec<String>,
    /// Working directory, with where it came from. Must exist and be a
    /// directory if provided; otherwise defaults to the daemon's current
    /// directory.
    pub cwd: Option<RequestedCwd>,
    /// Display name. Defaults to `command[0]`'s file stem, numbered when
    /// taken.
    pub name: Option<String>,
    /// What to do when a supplied `name` is taken. Unused without a `name`:
    /// a default name is always numbered.
    pub on_name_taken: OnNameTaken,
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
    /// The `[delivery]` table of the profile this window was launched from.
    /// `None` for a window the profile did not open or one whose profile
    /// declares none: its doors answer by the daemon's own defaults.
    pub delivery: Option<ProfileDelivery>,
    /// How the window's CLI comes to run the daemon's hooks. Given by the
    /// caller as planned and settled by [`WindowRegistry`] at spawn, from
    /// what the spawned argv really carries ([`HooksRoute::settle`]).
    pub hooks: HooksRoute,
    /// The profile's own program, run to wake the window's CLI when its
    /// `[delivery]` table wakes by command.
    pub wake_command: Option<WakeCommand>,
    /// How the window's credentials reach its CLI. Given by the caller as
    /// planned; with `Launch` the child's environment holds none of them.
    pub credentials: CredentialsMode,
    /// The folder planned for a `launch` window's credentials and settings
    /// ([`plan_credentials_folder`]); the open creates it there. `None` when
    /// the window has no usable place for one.
    pub credentials_dir: Option<PathBuf>,
}

/// How a window's CLI comes to run the daemon's hooks: the one fact both
/// whether the window expects status reports and whether it takes part in
/// delivery are read from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HooksRoute {
    /// The daemon arranged no hooks: no reports are expected, and a declared
    /// `[delivery]` relies on the CLI's own configuration.
    #[default]
    Unarranged,
    /// The launch's `[hooks]` injection. Settled at spawn: it stays only
    /// when the spawned argv still carries the injection.
    Injection,
    /// Hooks the CLI reads from its own settings, whose registered entries
    /// were read and held at launch.
    RegisteredEntries,
    /// The daemon arranged hooks, but they do not reach the CLI: their
    /// registration did not hold, their settings could not be written, the
    /// command interpreter would have reinterpreted them, or there was
    /// nowhere to write them.
    LeftOut,
}

impl HooksRoute {
    /// The route as it really is once the argv is spawned:
    /// `hooks_in_argv` is whether a `[hooks]` injection is in it.
    #[must_use]
    pub fn settle(self, hooks_in_argv: bool) -> Self {
        match self {
            _ if hooks_in_argv => Self::Injection,
            Self::Injection => Self::LeftOut,
            other => other,
        }
    }

    /// Whether the CLI runs hooks the daemon arranged, so it is expected to
    /// report its turns.
    #[must_use]
    pub fn reaches(self) -> bool {
        matches!(self, Self::Injection | Self::RegisteredEntries)
    }
}

/// Whether what the daemon sends of its own accord can reach a CLI: its
/// profile declares delivery (`has_delivery`), and the `hooks` the daemon
/// arranged were not left out. The one rule [`WindowEntry::delivers`] and the
/// choice made before a launch both read.
#[must_use]
pub(crate) fn takes_delivery(has_delivery: bool, hooks: HooksRoute) -> bool {
    has_delivery && hooks != HooksRoute::LeftOut
}

/// The program a profile's wake command runs, and the environment it runs
/// under: the profile's own argv without launch options, and its `[env]`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct WakeCommand {
    /// The program and its own arguments; the wake arguments follow them.
    pub argv: Vec<String>,
    /// The profile's `[env]` table.
    pub env: Vec<(String, String)>,
}

// An environment can carry tokens, so only its size is shown.
impl std::fmt::Debug for WakeCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WakeCommand")
            .field("argv", &self.argv)
            .field("env_len", &self.env.len())
            .finish()
    }
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

/// Which profile table an injection comes from. Only what tells them apart
/// in a log line lives here; the rules each table follows are shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionKind {
    /// The `[mcp]` table: points the CLI at the daemon's MCP bridge.
    Mcp,
    /// The `[hooks]` table: makes the CLI report its turn boundaries.
    Hooks,
    /// The `[credentials]` table's `launch_args`: arguments the CLI needs for
    /// its window credentials to stay out of what its tools run.
    LaunchArgs,
    /// The `[delivery]` table's `launch_prompt`: hands the CLI a first
    /// request at start-up.
    LaunchPrompt,
    /// The `[delivery]` table's `launch_context`: hands a chat participant
    /// the room's introduction at start-up.
    LaunchContext,
}

impl InjectionKind {
    /// The name log lines use for this kind.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Mcp => "MCP",
            Self::Hooks => "hooks",
            Self::LaunchArgs => "launch arguments",
            Self::LaunchPrompt => "launch prompt",
            Self::LaunchContext => "launch context",
        }
    }
}

/// Argv elements a profile's `[mcp]`, `[hooks]` or `[delivery]` table
/// contributes to one launch, already rendered, plus the settings file they
/// may name.
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
    /// The settings directory `args` refer to, written before spawn when
    /// absent. If either it or `config_file` cannot be written, the whole
    /// injection is dropped.
    pub config_dir: Option<InjectedConfigDir>,
}

/// A settings directory a [`CliInjection`] needs on disk before spawn, for a
/// CLI that takes its configuration only as a directory. The name is derived
/// from every file's name and content, so a directory already present under
/// that name already holds exactly these files and is never rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectedConfigDir {
    /// The directory the settings directory is created in.
    pub parent: PathBuf,
    /// The settings directory's name inside `parent`.
    pub dir_name: String,
    /// Its files, file name to content. Each name is a single plain file
    /// name ([`crate::profile::is_config_dir_file_name`]).
    pub files: BTreeMap<String, String>,
}

impl InjectedConfigDir {
    /// The settings directory's full path.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.parent.join(&self.dir_name)
    }
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
    /// The name the window was given, which differs from the requested one
    /// when it was numbered.
    pub name: String,
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
    /// See [`WindowEntry::delivers`].
    pub delivers: bool,
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
    /// The request's display name.
    Name,
}

impl RejectedArgument {
    /// The argument name clients read.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Cwd => "cwd",
            Self::Name => "name",
        }
    }
}

/// Why a window name cannot be used: the chat mention rule would not tell the
/// window apart from another, or would not reach it at all.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NameRefusal {
    /// A running window already carries a name the mention rule treats as
    /// the same.
    #[error("a window named “{0}” is already open")]
    InUse(String),
    /// The name is one of the reserved mentions.
    #[error("“{0}” is reserved for chat mentions")]
    Reserved(String),
}

/// Why [`WindowRegistry::rename_window`] changed nothing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WindowRenameError {
    /// The name fails [`validate_window_name`].
    #[error(transparent)]
    Invalid(#[from] WindowNameError),
    /// The name is taken or reserved.
    #[error(transparent)]
    NameTaken(#[from] NameRefusal),
}

/// Errors returned by [`WindowRegistry::open`], 1:1 with the IPC error codes
/// `invalid_argument` / `name_taken` / `spawn_failed` / `resource_exhausted`. The mapping
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
    /// The requested name is taken by a running window or reserved.
    #[error(transparent)]
    NameTaken(#[from] NameRefusal),
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
/// Enter in any keyboard protocol's form (see
/// [`crate::input_encode::contains_enter`]) submits nothing, so the output
/// that follows it is its own echo, regardless of whether it came from an
/// attached client or a message
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
            if crate::input_encode::contains_enter(data) {
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
    /// The send this call belongs to pressed the submit key on an earlier
    /// attempt, and the composer now reads empty with not one write from
    /// another writer since that key: the receiver took the envelope as one
    /// message. The body was not written again, and nothing of the send
    /// remains.
    Taken,
    /// The send this call belongs to pressed the submit key on an earlier
    /// attempt, but another writer may have written since that key, or an
    /// earlier write was left unconfirmed, so nothing tells whether the
    /// receiver took the envelope. The body was not written again: a send
    /// never writes its body again, nor presses the key on another writer's
    /// input, once it has pressed the submit key.
    Vanished,
}

/// Whether the send a [`WindowEntry::submit_message`] call belongs to has
/// already pressed the submit key on an earlier attempt. Kept apart from
/// [`ComposerResidue`], which a declined attempt can drop while the fact
/// that the key was pressed still holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendStage {
    /// No attempt of this send has pressed the submit key.
    BeforeSubmitKey,
    /// An earlier attempt of this send pressed the submit key, so its body is
    /// never written again.
    AfterSubmitKey,
}

/// What ended [`WindowEntry::wait_for_envelope_taken`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnvelopeWait {
    /// A finished screen that accepts a write read the composer empty, with
    /// nothing written since the submit key.
    Taken,
    /// Another writer wrote since the submit key.
    OtherWriter,
    /// The child exited.
    Exited,
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
    /// The body this send wants to send; the composer holds it followed by
    /// one space ([`ComposerLine`]).
    pub body: &'a str,
    /// [`WindowEntry::write_seq`] as it stood once this send's own writes
    /// were done. Still equal to the window's current count means not one
    /// write, from any writer, has landed since.
    pub write_seq: u64,
}

/// A [`ComposerResidue`] with its body turned into the line that was written,
/// so nothing past [`WindowEntry::submit_message`]'s entry sees a raw body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineResidue<'a> {
    line: &'a ComposerLine,
    write_seq: u64,
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
    /// The probes showed last in the composer, on a screen that accepts a
    /// write (see [`ProbeWait::Shown`]).
    Shown {
        behind: BehindProbe,
        shown: Vec<char>,
        at: u64,
        flight: Flight,
        observation: ComposerObservation,
    },
    /// Another writer wrote after the probe.
    Interrupted,
    /// The requester left, before the probe was written or before it showed.
    Abandoned,
    /// See [`ProbeWait::Undecidable`].
    Undecidable,
}

/// What [`WindowEntry::erase_probe`] found.
enum ProbeErased {
    /// The composer reads empty in this reading, with the ledger at this
    /// count.
    Emptied(ComposerObservation, u64),
    /// Another writer wrote; this call's own writes left the ledger here.
    Interrupted(u64),
    /// See [`ProbeWait::Undecidable`].
    Undecidable,
}

/// What [`WindowEntry::erase_probe_behind`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErasedBehind {
    /// Another writer wrote before the erase, which was not written.
    Moved,
    /// The erase was written and reached, or another writer wrote since.
    Erased,
    /// See [`ProbeWait::Undecidable`].
    Undecidable,
}

/// What [`WindowEntry::erase_probe_back_to`] ended as.
enum ErasedBack {
    /// The composer reads as the target again, the ledger at `at`.
    Restored { at: u64 },
    /// Another writer wrote; this call's own writes left the ledger at `at`.
    OtherWriter { at: u64 },
    /// The requester left with these probes possibly still in the composer.
    RequesterGone { at: u64, flight: Flight },
    /// See [`ProbeWait::Undecidable`].
    Undecidable,
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

impl<T> ComposerWait<T> {
    fn label(&self) -> &'static str {
        match self {
            Self::Done(_) => "reading",
            Self::OtherWriter => "other_writer",
            Self::Exited => "exited",
            Self::RequesterGone => "requester_gone",
        }
    }
}

/// Text the composer shows, as it was found.
#[derive(Debug, Clone)]
enum ShownText {
    /// The composer's whole text, found by its marker.
    Body(String),
    /// Only the end of a composer taller than the screen.
    Tail(String),
}

impl ShownText {
    fn of(readout: &ComposerReadout) -> Option<Self> {
        match readout {
            ComposerReadout::Body(text) => Some(Self::Body(text.clone())),
            ComposerReadout::Tail(text) => Some(Self::Tail(text.clone())),
            ComposerReadout::Empty | ComposerReadout::NotObserved => None,
        }
    }

    /// What shows in front of `probe` when this ends with it. Whitespace
    /// alone in front of it is no body that arrived. A tail that is the probe
    /// alone says nothing of what precedes it, which is off the screen.
    fn behind(&self, probe: char) -> Option<BehindProbe> {
        match self {
            Self::Body(text) => {
                let before = text.strip_suffix(probe)?;
                Some(if before.trim().is_empty() {
                    BehindProbe::Alone
                } else {
                    BehindProbe::Text(Self::Body(before.to_string()))
                })
            }
            Self::Tail(text) => text
                .strip_suffix(probe)
                .filter(|before| !before.trim().is_empty())
                .map(|before| BehindProbe::Text(Self::Tail(before.to_string()))),
        }
    }

    /// The last visible character of the text.
    fn end(&self) -> Option<char> {
        match self {
            Self::Body(text) | Self::Tail(text) => last_visible(text),
        }
    }

    /// Whether `other` can be another reading of the same text. A tail is
    /// redrawn from whichever row the screen's top lands on, so either one
    /// may be the end of the other.
    fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Body(a), Self::Body(b)) => composer_text_matches_ignoring_lost_whitespace(a, b),
            (Self::Tail(a), Self::Tail(b)) => tail_is_end_of(a, b) || tail_is_end_of(b, a),
            (Self::Body(_), Self::Tail(_)) | (Self::Tail(_), Self::Body(_)) => false,
        }
    }

    /// Whether `readout`, read once a probe that showed behind this was
    /// erased, shows the composer as it was again, `own` being the call's
    /// own text in it. Without the probe a body that ran off the screen may
    /// fit it after all.
    fn restored_in(&self, readout: &ComposerReadout, own: &ComposerLine) -> bool {
        let own = own.as_str();
        match (self, readout) {
            (Self::Body(before), ComposerReadout::Body(_)) => {
                composer_reads_back_as(readout, before)
            }
            (Self::Tail(_), ComposerReadout::Body(_)) => composer_reads_back_as(readout, own),
            (Self::Tail(_), ComposerReadout::Tail(text)) => tail_is_end_of(text, own),
            (Self::Body(_), ComposerReadout::Tail(_))
            | (
                Self::Body(_) | Self::Tail(_),
                ComposerReadout::Empty | ComposerReadout::NotObserved,
            ) => false,
        }
    }
}

/// What shows in front of a probe at the end of the composer. The receiver
/// handles input in order, so this is what everything written before the
/// probe left there.
#[derive(Debug, Clone)]
enum BehindProbe {
    /// The probe shows alone: nothing written before it is in the composer.
    Alone,
    /// The probe shows behind this text.
    Text(ShownText),
}

impl BehindProbe {
    /// What `readout` shows in front of `probe`, when it ends with it.
    fn of(readout: &ComposerReadout, probe: char) -> Option<Self> {
        ShownText::of(readout)?.behind(probe)
    }

    /// What the composer read as before the probe was written behind it.
    fn before(readout: &ComposerReadout) -> Option<Self> {
        match readout {
            ComposerReadout::Empty => Some(Self::Alone),
            ComposerReadout::Body(_) | ComposerReadout::Tail(_) => {
                ShownText::of(readout).map(Self::Text)
            }
            ComposerReadout::NotObserved => None,
        }
    }

    fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Alone, Self::Alone) => true,
            (Self::Text(a), Self::Text(b)) => a.same_as(b),
            (Self::Alone, Self::Text(_)) | (Self::Text(_), Self::Alone) => false,
        }
    }

    /// Whether `readout` shows this again.
    fn shows_again_in(&self, readout: &ComposerReadout) -> bool {
        match self {
            Self::Alone => matches!(readout, ComposerReadout::Empty),
            Self::Text(shown) => ShownText::of(readout).is_some_and(|read| shown.same_as(&read)),
        }
    }

    /// The last visible character of what shows in front of the probes.
    fn end(&self) -> Option<char> {
        match self {
            Self::Alone => None,
            Self::Text(shown) => shown.end(),
        }
    }
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

/// Reads what a fresh `line` and the `probe` written behind it left in the
/// composer, or `None` while that is not decided yet. A reading on its way to
/// the body and the probe -- a proper prefix of both together -- is not
/// decided, so a body made of or starting with the probe's character is never
/// taken for the probe alone.
fn reflection_of(
    readout: &ComposerReadout,
    line: &ComposerLine,
    probe: char,
) -> Option<BehindProbe> {
    let body = line.as_str();
    if let ComposerReadout::Body(text) = readout
        && is_proper_prefix_ignoring_whitespace(text, body, probe)
    {
        return None;
    }
    match BehindProbe::of(readout, probe)? {
        // A tail alone is not this call's text: it must end the body.
        BehindProbe::Text(ShownText::Tail(before)) if !tail_is_end_of(&before, body) => None,
        behind @ (BehindProbe::Alone | BehindProbe::Text(_)) => Some(behind),
    }
}

/// Whether `tail`, a reading that begins somewhere inside the composer's
/// text, is the end of `body` once whitespace is disregarded. The direction is
/// fixed and equality is not asked: a body taller than the screen is redrawn
/// from a different row each time.
fn tail_is_end_of(tail: &str, body: &str) -> bool {
    let shown: Vec<char> = tail.chars().filter(|c| !c.is_whitespace()).collect();
    let written: Vec<char> = body.chars().filter(|c| !c.is_whitespace()).collect();
    !shown.is_empty() && written.ends_with(&shown)
}

/// Whether `text` is a proper prefix of `body` followed by `probe`, once
/// whitespace is disregarded (see
/// [`composer_text_matches_ignoring_lost_whitespace`]).
fn is_proper_prefix_ignoring_whitespace(text: &str, body: &str, probe: char) -> bool {
    is_proper_prefix_ignoring_whitespace_of(text, &format!("{body}{probe}"))
}

/// Whether `text` is what this call wrote before a take-back (`written`, or
/// `written` short of its trailing probes), still showing: a reading the
/// take-back has not reached yet.
fn reads_as_before_take_back(text: &str, written: &str) -> bool {
    composer_text_matches_ignoring_lost_whitespace(text, written)
        || composer_text_matches_ignoring_lost_whitespace(text, split_own_probes(written).0)
}

/// `written` (a [`ComposerLine`], possibly with probes typed behind it) split
/// into the line and the probes. A line always ends with a space and a probe
/// is never blank, so the probes are whatever follows the last blank.
fn split_own_probes(written: &str) -> (&str, &str) {
    match written
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
    {
        Some((index, blank)) => written.split_at(index + blank.len_utf8()),
        None => ("", written),
    }
}

/// The last character of `text` that is not blank.
fn last_visible(text: &str) -> Option<char> {
    text.chars().rev().find(|c| !c.is_whitespace())
}

/// The last visible character a reading shows in the composer, if any.
fn readout_end(readout: &ComposerReadout) -> Option<char> {
    match readout {
        ComposerReadout::Body(text) | ComposerReadout::Tail(text) => last_visible(text),
        ComposerReadout::Empty | ComposerReadout::NotObserved => None,
    }
}

/// Reads a composer that ends with the last of the probes `typed` (in the
/// order they were typed): walking back from the end, every visible
/// character that is one of them, in an order they were typed in, is a probe
/// of this check's own; any one may be missing, taken by a dialog. Stops at
/// the first character that is not, which is where the composer's own text
/// ends: each probe was picked to differ from it. `None` while the last probe
/// typed has not shown, or -- with `expected`, the line this check wrote in
/// front of its probes -- while what is in front of them is on its way to
/// that line or a tail that does not end it.
fn count_own_probes(
    readout: &ComposerReadout,
    typed: &[char],
    expected: Option<&ComposerLine>,
) -> Option<(BehindProbe, Vec<char>)> {
    if readout_end(readout) != typed.last().copied() {
        return None;
    }
    let (behind, shown) = probes_behind(readout, typed)?;
    if let (Some(line), BehindProbe::Text(front)) = (expected, &behind) {
        let on_its_way = match front {
            ShownText::Body(front) => is_proper_prefix_ignoring_whitespace_of(front, line.as_str()),
            ShownText::Tail(front) => !tail_is_end_of(front, line.as_str()),
        };
        if on_its_way {
            return None;
        }
    }
    Some((behind, shown))
}

/// What shows in front of the probes of `typed` (in the order typed, any of
/// them missing) at the end of `readout`, and those probes. `None` for a
/// reading with no text, or a tail that shows nothing but probes.
fn probes_behind(readout: &ComposerReadout, typed: &[char]) -> Option<(BehindProbe, Vec<char>)> {
    let (text, tail) = match readout {
        ComposerReadout::Body(text) => (text, false),
        ComposerReadout::Tail(text) => (text, true),
        ComposerReadout::Empty | ComposerReadout::NotObserved => return None,
    };
    let mut unmatched = typed.len();
    let mut shown = Vec::new();
    let mut cut = text.len();
    for (index, c) in text.char_indices().rev() {
        if c.is_whitespace() {
            continue;
        }
        let Some(position) = typed[..unmatched].iter().rposition(|probe| *probe == c) else {
            break;
        };
        unmatched = position;
        shown.push(c);
        cut = index;
    }
    shown.reverse();
    let before = &text[..cut];
    let behind = match (before.trim().is_empty(), tail) {
        (true, false) => BehindProbe::Alone,
        // The probes alone at the end of a taller composer say nothing of what
        // precedes them, which is off the screen.
        (true, true) => return None,
        (false, false) => BehindProbe::Text(ShownText::Body(before.to_string())),
        (false, true) => BehindProbe::Text(ShownText::Tail(before.to_string())),
    };
    Some((behind, shown))
}

/// The reading of a fresh `line` and the probe typed behind it, until a
/// probe is typed again (see [`reflection_of`]).
fn reflection_settle(
    line: &ComposerLine,
) -> impl FnMut(&ComposerObservation, &[char]) -> Option<(BehindProbe, Vec<char>)> + '_ {
    move |observation, typed| {
        let probe = *typed.last()?;
        if !screen_accepts_write(observation) {
            return None;
        }
        reflection_of(&observation.readout, line, probe).map(|behind| (behind, vec![probe]))
    }
}

/// Whether `text` is a proper prefix of `body` once whitespace is
/// disregarded.
fn is_proper_prefix_ignoring_whitespace_of(text: &str, body: &str) -> bool {
    let shown: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    let written: Vec<char> = body.chars().filter(|c| !c.is_whitespace()).collect();
    shown.len() < written.len() && written.starts_with(&shown)
}

/// The probes one check has typed and not seen erased, in the order typed,
/// and the closed screens counted as of its latest decision (see
/// [`ComposerObservation::closed_screens`]).
#[derive(Debug, Clone, Default)]
struct Flight {
    typed: Vec<char>,
    closures: u64,
}

/// Whether [`WindowEntry::confirm_own_probes`] types the first probe itself
/// or the caller has already written what it waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FirstWrite {
    /// Picks the first probe and writes it at the check's ledger count.
    Probe,
    /// The caller's own write (an erase, a probe of an earlier call) left
    /// the ledger at the check's count.
    Written,
}

/// One wait on the daemon's own probes (see
/// [`WindowEntry::confirm_own_probes`]).
struct ProbeCheck<'a> {
    route: &'static str,
    /// The ledger count the first probe is written at, or the count the
    /// caller's own write left.
    at: u64,
    first: FirstWrite,
    flight: Flight,
    /// Characters the composer's own text may end with: never typed as a
    /// probe, so the probes and that text have one boundary.
    avoid: Vec<char>,
    /// The line written in front of the probes, when there is one.
    expected: Option<&'a ComposerLine>,
    /// Whether the probes are read by [`count_own_probes`] from the start,
    /// rather than by the caller's own reading until a probe is typed again.
    counted: bool,
    /// The composer reading the probes were typed behind, when the probes in
    /// flight -- those `flight` starts with included -- are recorded as
    /// unconfirmed probes of the call's own.
    unconfirmed: Option<&'a ComposerReadout>,
}

/// What ended [`WindowEntry::confirm_own_probes`].
enum ProbeWait {
    /// The probes showed: `behind` is what is in front of them and `shown`
    /// the probes of this check's own at the end, in typed order.
    Shown {
        behind: BehindProbe,
        shown: Vec<char>,
        at: u64,
        flight: Flight,
        observation: ComposerObservation,
    },
    /// The first probe was not written: another writer wrote first.
    NotWritten,
    /// Another writer wrote; this check's own writes left the ledger at `at`.
    OtherWriter { at: u64 },
    /// The requester left with these probes still on their way, already
    /// left for a later reading to count (see
    /// [`WindowEntry::leave_unseen_probes`]).
    RequesterGone { at: u64, flight: Flight },
    /// A screen closed the gate since the latest probe, which has not shown,
    /// and no probe character is left to type behind it: where the write
    /// went cannot be told from the screen.
    Undecidable,
}

/// What [`WindowEntry::confirm_own_probes`] does on one reading.
#[derive(Debug)]
enum ProbeStep {
    /// The probes are read: what is in front of them, and the own probes at
    /// the end.
    Decided(BehindProbe, Vec<char>),
    /// A screen closed the gate since the latest probe, which has not shown:
    /// type this probe, as of this many closed screens.
    Retype(char, u64),
    /// As [`ProbeStep::Retype`], with no probe character left.
    Undecidable,
}

/// The step one reading calls for, `decided` being what the reading made of
/// the probes. Undecided, a reading that accepts a write after more closed
/// screens than `flight` has seen calls for the next probe character.
fn probe_step(
    observation: &ComposerObservation,
    probes: &ProbeChars,
    flight: &Flight,
    avoid: &[char],
    decided: Option<(BehindProbe, Vec<char>)>,
) -> Option<ProbeStep> {
    if let Some((behind, shown)) = decided {
        return Some(ProbeStep::Decided(behind, shown));
    }
    let reopened = observation.closed_screens > flight.closures
        && screen_accepts_write(observation)
        && observation.readout != ComposerReadout::NotObserved;
    if !reopened {
        return None;
    }
    Some(match probes.pick(&flight.typed, avoid) {
        Some(probe) => ProbeStep::Retype(probe, observation.closed_screens),
        None => ProbeStep::Undecidable,
    })
}

/// What the write gate makes of one [`ComposerObservation`] (see
/// [`write_gate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteGate {
    /// A finished screen no rule closes: the daemon may write.
    Open,
    /// A rule reads the screen as an approval prompt, or as its explicit
    /// `unknown` declaration. A dialog keeps the cursor hidden even once it
    /// is drawn, so this holds whether or not the screen is finished.
    Closed,
    /// The program has not finished painting this screen: nothing is decided
    /// on it, neither a write nor a decline.
    Unfinished,
}

/// The one write gate every byte the daemon decides to write on its own --
/// a fresh body, the clear sequence, the submit key, a key-only resend, and a
/// `send_message` body written straight through -- passes before its first
/// byte. It takes the very [`ComposerObservation`] the decision to write was
/// made from, never a fresh one taken a moment later, so the check and the
/// write see the same screen. Every screen the detector reads closed is also
/// counted ([`ComposerObservation::closed_screens`]), so a dialog that came
/// and went between two readings is not missed.
fn write_gate(observation: &ComposerObservation) -> WriteGate {
    if matches!(
        observation.screen_rule,
        Some(DetectedStatus::WaitingApproval)
            | Some(DetectedStatus::Unknown(UnknownReason::DeclaredUnknown))
    ) {
        WriteGate::Closed
    } else if observation.painted {
        WriteGate::Open
    } else {
        WriteGate::Unfinished
    }
}

/// Whether [`write_gate`] is open on `observation`. A wait hands on only
/// readings whose gate is open or closed, never unfinished (see
/// [`WindowEntry::wait_for_composer`]), so a decision made from one that is
/// not open declines for a closed gate.
fn screen_accepts_write(observation: &ComposerObservation) -> bool {
    write_gate(observation) == WriteGate::Open
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
/// `reading::read_composer` trims each painted line's trailing whitespace
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

/// How a take-back ended: the composer confirmed empty (with the reading that
/// showed it), or a judgment declining the attempt.
/// `Emptied` also carries the ledger count the empty reading was guarded at.
enum TakenBack {
    Emptied(ComposerObservation, u64),
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
    /// Probes written behind a composer that read as `before`, not seen yet.
    UnconfirmedProbe {
        before: ComposerReadout,
        probes: Vec<char>,
    },
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
/// the probes it wrote). A reading `own` starts with counts as beside
/// something rather than gone, since part of `own` may be what is left.
fn own_text_shows(text: &str, own: &str) -> OwnShows {
    if reads_as_before_take_back(text, own) {
        return OwnShows::Alone;
    }
    let shown: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let core: String = split_own_probes(own)
        .0
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
    /// `spawn_exit_watcher`'s task. Taken (set to `None`) by
    /// [`WindowEntry::confirm_exited`] before it publishes `Exited`, so a
    /// reader that has seen `Exited` never finds a detector here.
    detector: Mutex<Option<Arc<DetectorHandle>>>,
    /// Kept even after `detector` is taken so [`WindowEntry::detector_task_alive`]
    /// can observe the driver task's teardown converging to zero strong
    /// references.
    detector_weak: Weak<DetectorHandle>,
    /// The confirmed status, its fan-out and the startup latch. Written only
    /// through [`WindowEntry::publish_confirmed`],
    /// [`WindowEntry::confirm_exited`] and the detector-loss backstop.
    status_gate: StatusGate,
    /// This window's own `StatusEvent` auth token, scoped to it. Held behind
    /// an [`Arc`] because
    /// [`DaemonConfig::status_token_override`] makes every window share one
    /// value; without the override each window owns a freshly generated token.
    /// No accessor exposes the raw value:
    /// [`WindowEntry::verify_status_token`] and the env injection in
    /// [`WindowRegistry::open`] are the only consumers. Empty for a window
    /// being taken back until its holder returns the token in its launch
    /// record.
    status_token: TokenSlot,
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
    /// The write ledger's count right after probes whose requester left
    /// before they showed (see [`InputProfile::probe`]), the probes in the
    /// order typed, and the closed screens counted as of the latest of them:
    /// if they show later, with nothing written since, what the composer
    /// holds can still be settled.
    unanswered_probe: Mutex<Option<(u64, Vec<char>, u64)>>,
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
    /// Whether a delivery agent is running for this window. The agent itself
    /// lives in the IPC layer and is started through
    /// [`WindowRegistry::wake_notices`].
    agent_running: Mutex<bool>,
    /// Wakes this window's running delivery agent.
    notice_wake: Notify,
    /// How many deliveries this window's agent has attempted, written or not.
    notice_attempts: AtomicU64,
    /// The parcels waiting for this window's CLI to read them through the
    /// doors its hooks run.
    inbox: Inbox,
    /// The identifier the window's CLI reported for its own session, as the
    /// daemon keeps it ([`WindowEntry::record_session_id`]).
    session_id: Mutex<Option<String>>,
    /// Fixed at open time and never rewritten, so a later request can never
    /// move who this window reports to.
    provenance: WindowProvenance,
    /// How many live connections have identified as this window. Shared with
    /// each [`McpConnectionClaim`] so a claim never keeps the entry alive.
    mcp_connections: Arc<AtomicUsize>,
    /// Raised when this window's display name or exit changes who takes part
    /// in a chat room.
    participation: Arc<ParticipationSignal>,
    /// The window's row in the window table, removed before the window's
    /// holder is told to end ([`WindowEntry::remove_row_then`]).
    row: WindowRow,
}

impl WindowEntry {
    /// This window's registry-assigned identifier: `w` and eight random
    /// base36 digits, for example `"wk3f9xa0q"`.
    ///
    /// Opaque, and the same for the window's whole life: nothing of the
    /// daemon run that opened the window is in it.
    pub fn window_id(&self) -> &str {
        &self.window_id
    }

    /// Removes the window's row from the window table and, only once it is
    /// gone, runs `end`, which tells the window's holder to end.
    pub(crate) async fn remove_row_then<T>(&self, end: impl std::future::Future<Output = T>) -> T {
        self.row.remove_then(end).await
    }

    /// Writes the window's values as they are now to its row.
    fn update_row(&self) {
        self.row.update(|| WindowValues {
            layout_json: self.layout_json(),
            session_id: self.session_id(),
            ..window_values(
                &self.provenance,
                &lock_recover(&self.name),
                &self.cwd,
                self.vendor.as_ref(),
                &self.command,
                self.detection.as_ref(),
            )
        });
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

    /// Marks a delivery agent as running for this window. `false` when one
    /// already is.
    pub(crate) fn claim_notice_agent(&self) -> bool {
        !std::mem::replace(&mut *lock_recover(&self.agent_running), true)
    }

    /// Ends this window's delivery agent.
    pub(crate) fn release_notice_agent(&self) {
        *lock_recover(&self.agent_running) = false;
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
    /// `data` is exactly this window's configured interrupt bytes, as
    /// [`crate::control::interrupt_window`] writes them. GUI and MCP input
    /// goes through [`WindowEntry::write_client_input`] instead.
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
        let interrupt_keys = !data.is_empty() && data == self.interrupt_bytes.as_slice();
        self.write_pty_bytes(data, origin, interrupt_keys).await
    }

    /// Writes GUI or MCP input to this window's PTY, as
    /// [`WindowEntry::write_pty_input`] does, judging the interrupt keys by
    /// [`crate::input_encode::ClientWrite::is_interrupt_key`]: a key press by
    /// the press, since its written bytes (key records, the Kitty form) need
    /// not equal the configured interrupt bytes; any other input never.
    ///
    /// # Errors
    ///
    /// As [`WindowEntry::write_pty_input`].
    pub async fn write_client_input(
        &self,
        write: &crate::input_encode::ClientWrite,
        origin: InputOrigin,
    ) -> crate::pty::PtyResult<()> {
        self.write_pty_bytes(write.bytes(), origin, write.is_interrupt_key())
            .await
    }

    async fn write_pty_bytes(
        &self,
        data: &[u8],
        origin: InputOrigin,
        interrupt_keys: bool,
    ) -> crate::pty::PtyResult<()> {
        // Scoped so the `std::sync::Mutex` guard is released before the
        // `.await` below (`WindowEntry::detector`).
        {
            let guard = lock_recover(&self.detector);
            if let Some(detector) = guard.as_ref() {
                detector.notify_input(classify_input_echo(data, origin));
            }
        }
        let prefix = if interrupt_keys {
            TakeBackPrefix::Withhold
        } else {
            TakeBackPrefix::Apply
        };
        self.session.write_input_with(data, prefix).await
    }

    /// What [`crate::control::interrupt_window`] writes to this window.
    #[must_use]
    pub fn interrupt_bytes(&self) -> &[u8] {
        &self.interrupt_bytes
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

    /// Notes that the running call read its own `line` back off the screen,
    /// when `shown` is it.
    fn note_read_back(&self, shown: &str, line: &ComposerLine) {
        if composer_text_matches_ignoring_lost_whitespace(shown, line.as_str()) {
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
        let _focus = self.session.open_focus_interval();
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
        if !screen_accepts_write(&observation) {
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
    /// writer's write ends that wait without an answer from the screen. A
    /// dialog that may have taken a probe gets the next probe character typed
    /// behind it once the screen accepts input again; with every one typed,
    /// the call declines and the window takes no further write of the
    /// daemon's until another writer writes (see
    /// [`WindowEntry::confirm_own_probes`]).
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
    /// `stage` says whether an earlier attempt of the same send pressed the
    /// submit key. After it, the body is never written again, and the call
    /// decides on the finished reading alone (see
    /// [`WindowEntry::judge_after_submit_key`]): only the submit key, behind a
    /// probe when the envelope shows folded or by its end, is ever written.
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
        stage: SendStage,
        requester: Option<watch::Receiver<bool>>,
    ) -> PtyResult<SubmitJudgment> {
        let _focus = self.session.open_focus_interval();
        let mut requester = Requester(requester);
        let Some(_guard) = self.lock_input(&mut requester).await else {
            return Ok(declined(NotPlacedReason::Unobservable, None));
        };
        let _scope = self.begin_take_back_scope();
        if !self.composer_access.is_judged() || self.input.probe.is_none() {
            return Ok(declined(NotPlacedReason::NoComposer, None));
        }
        // The one place the bodies become what is written: everything below
        // decides on, writes and takes back the lines alone.
        let line = ComposerLine::new(body);
        let residue_line = residue.map(|residue| ComposerLine::new(residue.body));
        let residue = residue
            .zip(residue_line.as_ref())
            .map(|(residue, line)| LineResidue {
                line,
                write_seq: residue.write_seq,
            });
        let judgment = self
            .judge_submit(&line, residue, stage, &mut requester)
            .await?;
        self.check_after_another_write(judgment, &mut requester)
            .await
    }

    /// [`WindowEntry::submit_message`]'s judgment, before what another
    /// writer's write leaves is checked.
    async fn judge_submit(
        &self,
        line: &ComposerLine,
        residue: Option<LineResidue<'_>>,
        stage: SendStage,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        // The ledger as of the reading every write below is decided on: read
        // before asking for it, so a write landing meanwhile is caught by the
        // first ledger-checked write.
        let mut seen = self.write_seq();
        let Some(mut observation) = self.observe_composer(requester).await? else {
            return Ok(declined(NotPlacedReason::Unobservable, None));
        };
        let vouched_now = || {
            residue
                .filter(|residue| self.record_vouches_for_residue(Some(*residue), line))
                .map(|residue| residue.write_seq)
        };
        if matches!(observation.readout, ComposerReadout::NotObserved) {
            // A dialog the screen rule names hides the composer for good: the
            // gate is closed on that screen, so nothing is written and there
            // is nothing to wait out.
            if let (false, Some(vouched_seq)) = (screen_accepts_write(&observation), vouched_now())
            {
                return Ok(decline_not_accepting(Some(vouched_seq)));
            }
            // A screen caught between erasing and repainting its composer
            // reads as unobservable, and nothing is decided on that, however
            // the ledger vouches. What the wait reads is judged from the top.
            let readable = |observation: &ComposerObservation| {
                (!matches!(observation.readout, ComposerReadout::NotObserved))
                    .then(|| observation.clone())
            };
            let guarded = self.write_seq();
            match self
                .wait_for_composer("judge_submit", Some(guarded), requester, readable)
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
        let vouched_seq = vouched_now();
        if stage == SendStage::AfterSubmitKey
            && let Some(judgment) = self.judge_after_submit_key(&observation, vouched_seq)
        {
            return Ok(judgment);
        }
        if let (ComposerReadout::Empty, Some(vouched_seq)) = (&observation.readout, vouched_seq) {
            if self.degenerate_and_unchanged() {
                return Ok(declined(NotPlacedReason::Unobservable, None));
            }
            return self
                .confirm_unshown_envelope(line, &observation, vouched_seq, stage, requester)
                .await;
        }
        match &observation.readout {
            ComposerReadout::Empty => {
                // An unconfirmed write may still be on its way: anything
                // written now, a probe included, would land on top of it.
                if self.degenerate_and_unchanged() {
                    return Ok(declined(NotPlacedReason::Unobservable, None));
                }
                self.confirm_empty_then_write(line, &observation, seen, stage, requester)
                    .await
            }
            ComposerReadout::Body(_) | ComposerReadout::Tail(_) => {
                // Only a body found by its marker can be the caller's own
                // envelope read back; the end of a taller-than-screen
                // composer can show anything that ends alike.
                let marked = match &observation.readout {
                    ComposerReadout::Body(text) => Some(text),
                    ComposerReadout::Tail(_)
                    | ComposerReadout::Empty
                    | ComposerReadout::NotObserved => None,
                };
                if let Some(text) = marked {
                    match self.settle_unanswered_probe(text, &observation).await? {
                        Some(TakenBack::Emptied(settled, seq)) => {
                            return self
                                .write_fresh_and_decide(line, &settled, seq, stage, requester)
                                .await;
                        }
                        Some(TakenBack::Declined(judgment)) => return Ok(judgment),
                        None => {}
                    }
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
                let restored_own_envelope = marked.is_some_and(|text| {
                    residue.is_some_and(|residue| {
                        composer_text_matches_ignoring_lost_whitespace(text, residue.line.as_str())
                            && residue.line == line
                    })
                });
                if let (true, Some(text)) = (restored_own_envelope, marked) {
                    self.note_read_back(text, line);
                }
                let vouched_seq = residue
                    .filter(|residue| self.record_vouches_for_residue(Some(*residue), line))
                    .map(|residue| residue.write_seq);
                if restored_own_envelope || vouched_seq.is_some() {
                    // A dialog's option row can read exactly like a composer
                    // holding other text (an approval screen's `1. Yes`, most
                    // notably), so the ledger match above cannot by itself
                    // tell that apart from the caller's own envelope. The
                    // screen rule can: it names the dialog, not the row's
                    // shape.
                    if !screen_accepts_write(&observation) {
                        return Ok(decline_not_accepting(residue.map(|r| r.write_seq)));
                    }
                    match (restored_own_envelope, vouched_seq) {
                        (true, Some(vouched_seq)) => {
                            self.resend_key_only(line, vouched_seq, requester).await
                        }
                        // Someone wrote since the caller's baseline, so the
                        // envelope on screen may have unshown input behind it.
                        (true, None) => {
                            self.confirm_envelope_then_resend(
                                line,
                                &observation,
                                seen,
                                false,
                                stage,
                                requester,
                            )
                            .await
                        }
                        // The ledger alone is no evidence of what shows: a
                        // folded placeholder, a dialog row or an echo can read
                        // as the composer. The probe has to show behind it.
                        (false, Some(vouched_seq)) => {
                            self.confirm_envelope_then_resend(
                                line,
                                &observation,
                                vouched_seq,
                                true,
                                stage,
                                requester,
                            )
                            .await
                        }
                        // The branch is entered on one of the two; fail closed.
                        (false, None) => Ok(declined(NotPlacedReason::Unobservable, None)),
                    }
                } else if let Some(residue) =
                    // The ledger alone vouches the composer holds only the
                    // caller's own text, however the CLI draws it (folded or
                    // not); the probe after the take-back confirms it emptied.
                    residue.filter(|residue| residue.write_seq == self.write_seq())
                {
                    self.take_back_own_residue_then_write(
                        line,
                        residue,
                        &observation,
                        stage,
                        requester,
                    )
                    .await
                } else if !self.degenerate_and_unchanged() {
                    // What reads as someone else's text may only be a screen
                    // that has not caught up with input that emptied it.
                    self.confirm_empty_then_write(line, &observation, seen, stage, requester)
                        .await
                } else {
                    Ok(foreign_draft(declined(NotPlacedReason::Foreign, None)))
                }
            }
            // Settled by the wait above.
            ComposerReadout::NotObserved => Ok(declined(NotPlacedReason::Unobservable, None)),
        }
    }

    /// The judgment of an attempt made after the send pressed the submit key,
    /// on a finished reading and the ledger as of it (`vouched_seq`: nothing
    /// but the send's own writes since). All that matters then is whether
    /// the envelope is still in the composer; a probe written to find out
    /// would race the receiver taking it. `None` leaves the call to resend
    /// the key alone, or behind a probe when the envelope does not show as
    /// itself.
    fn judge_after_submit_key(
        &self,
        observation: &ComposerObservation,
        vouched_seq: Option<u64>,
    ) -> Option<SubmitJudgment> {
        // Another writer's input may be in the composer: the key is not
        // pressed on it, and an empty composer proves nothing.
        let Some(vouched_seq) = vouched_seq else {
            return Some(declined(NotPlacedReason::Vanished, None));
        };
        if self.degenerate_and_unchanged() {
            return Some(declined(NotPlacedReason::Vanished, None));
        }
        match observation.readout {
            ComposerReadout::Empty if !screen_accepts_write(observation) => {
                Some(decline_not_accepting(Some(vouched_seq)))
            }
            ComposerReadout::Empty => Some(declined(NotPlacedReason::Taken, None)),
            ComposerReadout::Body(_) | ComposerReadout::Tail(_) | ComposerReadout::NotObserved => {
                None
            }
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
            OwnLeft::Text(text) => Ok(match self.check_own_text(text, &own, requester).await? {
                Checked::Gone => declined(NotPlacedReason::Interrupted, None),
                Checked::MayRemain => remaining(judgment),
            }),
            OwnLeft::UnconfirmedProbe { before, probes } => {
                if !requester.has_left() {
                    self.check_unconfirmed_probe(before, probes, requester)
                        .await?;
                }
                Ok(judgment)
            }
        }
    }

    /// The first reading of a screen that accepts a write and shows a
    /// composer, with the ledger count it was guarded at: a frame caught
    /// between erasing and repainting the composer says nothing of what is
    /// left. There is no time limit: `None` once another writer writes or
    /// `requester` leaves first.
    async fn read_writable_screen(
        &self,
        route: &'static str,
        requester: &mut Requester,
    ) -> PtyResult<Option<(ComposerObservation, u64)>> {
        let seen = self.write_seq();
        match self
            .wait_for_composer(route, Some(seen), requester, |observation| {
                (screen_accepts_write(observation)
                    && observation.readout != ComposerReadout::NotObserved)
                    .then(|| observation.clone())
            })
            .await
        {
            ComposerWait::Done(observation) => Ok(Some((observation, seen))),
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
        requester: &mut Requester,
    ) -> PtyResult<Checked> {
        let gone_if_read_back = if part.read_back {
            Checked::Gone
        } else {
            Checked::MayRemain
        };
        let Some((observation, seen)) = self
            .read_writable_screen("check_own_text", requester)
            .await?
        else {
            return Ok(Checked::MayRemain);
        };
        let shows = match &observation.readout {
            ComposerReadout::Empty => OwnShows::Absent,
            ComposerReadout::NotObserved | ComposerReadout::Tail(_) => {
                return Ok(Checked::MayRemain);
            }
            ComposerReadout::Body(text) => own_text_shows(text, own),
        };
        let only_interrupt_keys_since = part.after_own_write.map(|after| after.wrapping_add(1))
            == Some(seen)
            && self.session.latest_withheld_write() == Some(seen);
        if only_interrupt_keys_since
            && matches!(observation.readout, ComposerReadout::Body(_))
            && matches!(shows, OwnShows::Absent | OwnShows::WithOthers)
        {
            return self.take_back_own(own, seen, &observation).await;
        }
        match shows {
            OwnShows::Absent => return Ok(gone_if_read_back),
            OwnShows::WithOthers => return Ok(Checked::MayRemain),
            OwnShows::Alone => {}
        }
        let ProbeReading::Shown {
            behind,
            shown,
            at,
            flight,
            observation: probed,
        } = self
            .read_through_probe(seen, &observation, &[], requester)
            .await?
        else {
            return Ok(Checked::MayRemain);
        };
        let gone_once_erased = |erased: ErasedBehind, gone: Checked| match erased {
            ErasedBehind::Undecidable => Checked::MayRemain,
            ErasedBehind::Moved | ErasedBehind::Erased => gone,
        };
        let before = match &behind {
            BehindProbe::Text(ShownText::Body(before))
                if !before.chars().all(char::is_whitespace) =>
            {
                before
            }
            BehindProbe::Alone | BehindProbe::Text(ShownText::Body(_)) => {
                let erased = self
                    .erase_probe_behind(at, shown.len(), flight, &behind)
                    .await?;
                return Ok(gone_once_erased(erased, Checked::Gone));
            }
            // The end of a taller composer cannot show the call's own text
            // alone.
            BehindProbe::Text(ShownText::Tail(_)) => {
                self.erase_probe_behind(at, shown.len(), flight, &behind)
                    .await?;
                return Ok(Checked::MayRemain);
            }
        };
        match own_text_shows(before, own) {
            OwnShows::Alone => {
                let written: String = own.chars().chain(shown.iter().copied()).collect();
                self.take_back_own(&written, at, &probed).await
            }
            OwnShows::WithOthers => {
                self.erase_probe_behind(at, shown.len(), flight, &behind)
                    .await?;
                Ok(Checked::MayRemain)
            }
            OwnShows::Absent => {
                let erased = self
                    .erase_probe_behind(at, shown.len(), flight, &behind)
                    .await?;
                Ok(gone_once_erased(erased, gone_if_read_back))
            }
        }
    }

    /// [`WindowEntry::take_back_and_settle`] of the call's own `written`,
    /// as what the check found of it.
    async fn take_back_own(
        &self,
        written: &str,
        seq_after_written: u64,
        before_take_back: &ComposerObservation,
    ) -> PtyResult<Checked> {
        Ok(
            match self
                .take_back_and_settle(written, seq_after_written, before_take_back)
                .await?
            {
                TakenBack::Emptied(..) => Checked::Gone,
                TakenBack::Declined(judgment) if leaves_envelope(&judgment) => Checked::MayRemain,
                TakenBack::Declined(_) => Checked::Gone,
            },
        )
    }

    /// Once another writer's write ended the call while its `probes`, written
    /// behind a composer reading as `before_probe`, had not shown: erases
    /// them when the screen shows them right behind `before_probe` -- the end
    /// of a taller composer included -- and writes nothing otherwise (they
    /// may sit among the other writer's text). The other writer's bytes are
    /// not told apart from the probes: a character of theirs at the end that
    /// equals one of the probes is erased as one.
    async fn check_unconfirmed_probe(
        &self,
        before_probe: &ComposerReadout,
        probes: &[char],
        requester: &mut Requester,
    ) -> PtyResult<()> {
        let Some(before) = BehindProbe::before(before_probe) else {
            return Ok(());
        };
        let Some((observation, seen)) = self
            .read_writable_screen("check_unconfirmed_probe", requester)
            .await?
        else {
            return Ok(());
        };
        let probes_show = &observation.readout != before_probe
            && probes_behind(&observation.readout, probes)
                .is_some_and(|(front, shown)| !shown.is_empty() && front.same_as(&before));
        if !probes_show {
            return Ok(());
        }
        let ProbeReading::Shown {
            behind,
            shown,
            at,
            flight,
            observation: probed,
        } = self
            .read_through_probe(seen, &observation, probes, requester)
            .await?
        else {
            return Ok(());
        };
        // The probes this check typed showed, so every probe at the end --
        // the earlier ones included -- is counted at once, and all of them
        // are erased when they show right behind `before`.
        let (count, rest) = match probes_behind(&probed.readout, &flight.typed) {
            Some((rest, all)) if rest.same_as(&before) => (all.len(), rest),
            Some(_) | None => (shown.len(), behind),
        };
        self.erase_probe_behind(at, count, flight, &rest).await?;
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
            Some(detector) => detector.query_composer().await,
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
            .wait_for_composer("observe_composer", None, requester, |observation| {
                Some(observation.clone())
            })
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
    /// [`DetectorHandle::query_composer`]). Each reading carries how many
    /// closed screens the detector has read so far, whether or not this wait
    /// saw them ([`ComposerObservation::closed_screens`]).
    ///
    /// A reading of a screen the program has not finished painting (see
    /// [`WriteGate::Unfinished`]) never reaches `settle`: it is read again,
    /// so everything decided from what a wait hands on sees a finished
    /// screen, whatever clock the detector answered on.
    ///
    /// A wait that turns down an unreadable composer is logged under `route`
    /// when it ends other than by a reading: only another writer, the
    /// requester leaving or the child exiting ends such a wait, so this is
    /// what tells a window held by one apart.
    async fn wait_for_composer<T>(
        &self,
        route: &'static str,
        own_seq: Option<u64>,
        requester: &mut Requester,
        mut settle: impl FnMut(&ComposerObservation) -> Option<T>,
    ) -> ComposerWait<T> {
        let mut exit = self.session.exit_notified();
        let mut writes = self.session.subscribe_writes_recorded();
        let mut arrivals = self.session.output_arrivals();
        let mut status = self.watch_status();
        let mut waiting_on_unreadable = false;
        let mut waiting_on_unfinished = false;
        let end = 'wait: loop {
            // Seen before the query, so a change while it runs is a cue.
            arrivals.borrow_and_update();
            status.borrow_and_update();
            if let Some(end) = self.wait_ended(&exit, requester, own_seq) {
                break 'wait end;
            }
            let observation = tokio::select! {
                biased;
                () = child_exited(&mut exit) => break 'wait ComposerWait::Exited,
                () = requester.left() => break 'wait ComposerWait::RequesterGone,
                () = ledger_moved(&mut writes, own_seq) => break 'wait ComposerWait::OtherWriter,
                observation = self.query_composer() => observation,
            };
            if let Some(end) = self.wait_ended(&exit, requester, own_seq) {
                break 'wait end;
            }
            if write_gate(&observation) == WriteGate::Unfinished {
                if !waiting_on_unfinished {
                    waiting_on_unfinished = true;
                    tracing::debug!(
                        window_id = %self.window_id(),
                        route,
                        "waiting for the screen to finish painting"
                    );
                }
            } else if let Some(settled) = settle(&observation) {
                break 'wait ComposerWait::Done(settled);
            }
            if !waiting_on_unreadable && observation.readout == ComposerReadout::NotObserved {
                waiting_on_unreadable = true;
                tracing::debug!(
                    window_id = %self.window_id(),
                    route,
                    "waiting for the composer to become readable"
                );
            }
            tokio::select! {
                biased;
                () = child_exited(&mut exit) => break 'wait ComposerWait::Exited,
                () = requester.left() => break 'wait ComposerWait::RequesterGone,
                () = ledger_moved(&mut writes, own_seq) => break 'wait ComposerWait::OtherWriter,
                () = next_change(&mut arrivals) => {}
                () = next_change(&mut status) => {}
            }
        };
        if waiting_on_unreadable {
            // A torn frame is routine; only a wait no reading ended is news.
            match &end {
                ComposerWait::Done(_) => tracing::debug!(
                    window_id = %self.window_id(),
                    route,
                    ended_by = end.label(),
                    "wait on an unreadable composer ended"
                ),
                ComposerWait::OtherWriter | ComposerWait::Exited | ComposerWait::RequesterGone => {
                    tracing::info!(
                        window_id = %self.window_id(),
                        route,
                        ended_by = end.label(),
                        "wait on an unreadable composer ended"
                    )
                }
            }
        }
        end
    }

    /// Resolves once the composer reads empty on a screen that accepts a
    /// write, or the child has exited. Writes by others do not end it: a
    /// draft being typed moves the ledger on every key and is still a draft.
    /// Output reaching the screen is what prompts each new reading, so an
    /// emptying whose redraw lags is still caught.
    pub(crate) async fn wait_for_composer_empty(&self) {
        match self
            .wait_for_composer(
                "wait_for_composer_empty",
                None,
                &mut Requester::none(),
                |observation| {
                    (screen_accepts_write(observation)
                        && observation.readout == ComposerReadout::Empty)
                        .then_some(())
                },
            )
            .await
        {
            // The caller's own loop sees an exited child.
            ComposerWait::Done(()) | ComposerWait::Exited => {}
            // Neither can end a wait with no ledger count and no requester.
            ComposerWait::OtherWriter | ComposerWait::RequesterGone => {}
        }
    }

    /// Waits, once a send has pressed the submit key leaving the ledger at
    /// `after_key`, for a finished screen that accepts a write to read the
    /// composer empty. The envelope showed in the composer when the key was
    /// pressed, so that reading means the receiver took it as one message.
    /// There is no time limit: the caller bounds it.
    pub(crate) async fn wait_for_envelope_taken(&self, after_key: u64) -> EnvelopeWait {
        match self
            .wait_for_composer(
                "wait_for_envelope_taken",
                Some(after_key),
                &mut Requester::none(),
                |observation| {
                    (screen_accepts_write(observation)
                        && observation.readout == ComposerReadout::Empty)
                        .then_some(())
                },
            )
            .await
        {
            ComposerWait::Done(()) => EnvelopeWait::Taken,
            // No requester was given, so it never leaves.
            ComposerWait::OtherWriter | ComposerWait::RequesterGone => EnvelopeWait::OtherWriter,
            ComposerWait::Exited => EnvelopeWait::Exited,
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
        route: &'static str,
        own_seq: u64,
        settle: impl FnMut(&ComposerObservation) -> Option<T>,
    ) -> PtyResult<Option<T>> {
        match self
            .wait_for_composer(route, Some(own_seq), &mut Requester::none(), settle)
            .await
        {
            ComposerWait::Done(settled) => Ok(Some(settled)),
            // No requester was given, so it never leaves.
            ComposerWait::OtherWriter | ComposerWait::RequesterGone => Ok(None),
            ComposerWait::Exited => Err(crate::pty::PtyError::ChildExited),
        }
    }

    /// `observed` may not show input written before it yet. Writes the probe
    /// behind that input and writes `line` only once the probe shows alone
    /// and is erased; a probe that shows behind anything else means someone's
    /// text is there, and only the probe is erased. Nothing of `line` is
    /// written on any declining path.
    async fn confirm_empty_then_write(
        &self,
        line: &ComposerLine,
        observed: &ComposerObservation,
        seq: u64,
        stage: SendStage,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        if !screen_accepts_write(observed) {
            return Ok(decline_not_accepting(None));
        }
        let unanswered = lock_recover(&self.unanswered_probe)
            .clone()
            .filter(|(at, ..)| *at == seq);
        if let Some((_, probes, closures)) = unanswered {
            // Earlier probes are still on their way. A second probe of the
            // same character would leave one of them in the composer; one of
            // another character tells, counted from the end, which of them
            // showed. With none left to type, they are waited for.
            let avoid: Vec<char> = readout_end(&observed.readout).into_iter().collect();
            let next = self
                .input
                .probe
                .as_ref()
                .and_then(|chars| chars.pick(&probes, &avoid));
            let (first, closures) = match next {
                Some(_) => (FirstWrite::Probe, observed.closed_screens),
                None => (FirstWrite::Written, closures),
            };
            let check = ProbeCheck {
                route: "confirm_empty_then_write",
                at: seq,
                first,
                flight: Flight {
                    typed: probes,
                    closures,
                },
                avoid,
                expected: None,
                counted: true,
                unconfirmed: Some(&observed.readout),
            };
            return match self
                .confirm_own_probes(
                    check,
                    requester,
                    |_| TakeBackRegistration::Empty,
                    |_, _| None,
                )
                .await?
            {
                ProbeWait::Shown {
                    behind: BehindProbe::Alone,
                    shown,
                    at,
                    flight,
                    ..
                } => {
                    self.write_once_probe_erased(line, at, shown.len(), flight, stage, requester)
                        .await
                }
                ProbeWait::Shown {
                    behind: before @ BehindProbe::Text(_),
                    shown,
                    at,
                    flight,
                    ..
                } => {
                    self.decline_foreign_behind_probes(at, shown.len(), flight, &before)
                        .await
                }
                ProbeWait::NotWritten | ProbeWait::OtherWriter { .. } => {
                    Ok(declined(NotPlacedReason::Interrupted, None))
                }
                ProbeWait::RequesterGone { .. } => {
                    Ok(declined(NotPlacedReason::Unobservable, None))
                }
                ProbeWait::Undecidable => Ok(self.declined_undecidable()),
            };
        }
        match self
            .read_through_probe(seq, observed, &[], requester)
            .await?
        {
            ProbeReading::Shown {
                behind: BehindProbe::Alone,
                shown,
                at,
                flight,
                ..
            } => {
                self.write_once_probe_erased(line, at, shown.len(), flight, stage, requester)
                    .await
            }
            ProbeReading::Shown {
                behind: before @ BehindProbe::Text(_),
                shown,
                at,
                flight,
                ..
            } => {
                self.decline_foreign_behind_probes(at, shown.len(), flight, &before)
                    .await
            }
            ProbeReading::Interrupted => Ok(declined(NotPlacedReason::Interrupted, None)),
            ProbeReading::Abandoned => Ok(declined(NotPlacedReason::Unobservable, None)),
            ProbeReading::Undecidable => Ok(self.declined_undecidable()),
        }
    }

    /// Erases `count` probes that showed behind someone else's text (ledger
    /// at `seq`) and declines for that text.
    async fn decline_foreign_behind_probes(
        &self,
        seq: u64,
        count: usize,
        flight: Flight,
        before: &BehindProbe,
    ) -> PtyResult<SubmitJudgment> {
        Ok(
            match self.erase_probe_behind(seq, count, flight, before).await? {
                ErasedBehind::Undecidable => self.declined_undecidable(),
                ErasedBehind::Moved | ErasedBehind::Erased => {
                    foreign_draft(declined(NotPlacedReason::Foreign, None))
                }
            },
        )
    }

    /// The ledger vouches that only the caller's own envelope was written
    /// since its baseline, yet the composer reads empty: the envelope is not
    /// drawn yet, or the receiver has already taken or dropped it. The probe
    /// written behind it tells which, in the order the receiver handles its
    /// input: shown behind something (behind the end of `line`, when only
    /// the end of a taller composer shows), the envelope is there and the
    /// submit key alone is pressed once the probe is erased; shown alone, it
    /// is gone and `line` is written fresh; nothing shown, or a screen that
    /// shows no composer, the call waits.
    async fn confirm_unshown_envelope(
        &self,
        line: &ComposerLine,
        observed: &ComposerObservation,
        seq: u64,
        stage: SendStage,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        if !screen_accepts_write(observed) {
            return Ok(decline_not_accepting(Some(seq)));
        }
        if requester.has_left() {
            return Ok(declined(NotPlacedReason::Unobservable, Some(seq)));
        }
        let check = ProbeCheck {
            route: "confirm_unshown_envelope",
            at: seq,
            first: FirstWrite::Probe,
            flight: Flight {
                typed: Vec::new(),
                closures: observed.closed_screens,
            },
            avoid: last_visible(line.as_str())
                .into_iter()
                .chain(readout_end(&observed.readout))
                .collect(),
            expected: Some(line),
            counted: false,
            unconfirmed: None,
        };
        match self
            .confirm_own_probes(
                check,
                requester,
                |typed| self.own_text(&line.with_probes(typed)),
                reflection_settle(line),
            )
            .await?
        {
            ProbeWait::Shown {
                behind: BehindProbe::Text(front),
                shown,
                at,
                flight,
                ..
            } => {
                if let ShownText::Body(before) = &front {
                    self.note_read_back(before, line);
                }
                self.erase_probe_then_resend(line, &front, at, shown.len(), flight, requester)
                    .await
            }
            ProbeWait::Shown {
                behind: BehindProbe::Alone,
                shown,
                at,
                flight,
                ..
            } => {
                self.write_once_probe_erased(line, at, shown.len(), flight, stage, requester)
                    .await
            }
            // The earlier attempt's envelope stays, carried forward as it was.
            ProbeWait::NotWritten => Ok(declined(NotPlacedReason::Interrupted, Some(seq))),
            ProbeWait::OtherWriter { .. } => Ok(declined_unvouched(NotPlacedReason::Interrupted)),
            ProbeWait::RequesterGone { .. } => {
                Ok(declined_unvouched(NotPlacedReason::Unobservable))
            }
            ProbeWait::Undecidable => Ok(self.declined_undecidable()),
        }
    }

    /// Erases `count` probes that show alone (ledger at `seq`) and writes
    /// `line` once the composer reads empty.
    async fn write_once_probe_erased(
        &self,
        line: &ComposerLine,
        seq: u64,
        count: usize,
        flight: Flight,
        stage: SendStage,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        match self.erase_probe(seq, count, flight).await? {
            ProbeErased::Emptied(settled, after_erase) => {
                self.write_fresh_and_decide(line, &settled, after_erase, stage, requester)
                    .await
            }
            ProbeErased::Interrupted(_) => Ok(declined(NotPlacedReason::Interrupted, None)),
            ProbeErased::Undecidable => Ok(self.declined_undecidable()),
        }
    }

    /// The composer may hold the caller's own envelope, but the screen does
    /// not show it as such: someone wrote since the caller's baseline, or the
    /// envelope is drawn as something else (`ledger_vouches`: the ledger says
    /// nothing but the envelope was written, yet a placeholder, a dialog row
    /// or the end of a long composer is what shows). Writes the probe behind
    /// it and presses the submit key only once the probe shows behind what
    /// counts as the envelope and, with the probe erased, the composer reads
    /// as it did before the probe again. The body is not written again.
    ///
    /// What counts as the envelope is the screen showing the body when the
    /// ledger does not vouch; a tail must end the body either way, since the
    /// end of a long composer can be anything that ends alike. A probe that
    /// shows alone means the envelope is not there, however the ledger
    /// vouches: the probe is erased and nothing is sent. After the submit key
    /// (`stage`), that is the receiver having taken the envelope, as long as
    /// nothing but the probe and its erase was written since.
    // What the probe means depends on both the ledger and whether the key was pressed.
    #[allow(clippy::too_many_arguments)]
    async fn confirm_envelope_then_resend(
        &self,
        line: &ComposerLine,
        observed: &ComposerObservation,
        seq: u64,
        ledger_vouches: bool,
        stage: SendStage,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        let envelope_shows = |shown: &ShownText| match shown {
            ShownText::Body(before) => {
                ledger_vouches
                    || composer_text_matches_ignoring_lost_whitespace(before, line.as_str())
            }
            ShownText::Tail(before) => tail_is_end_of(before, line.as_str()),
        };
        match self
            .read_through_probe(seq, observed, &[], requester)
            .await?
        {
            ProbeReading::Shown {
                behind: BehindProbe::Alone,
                shown,
                at,
                flight,
                ..
            } => match stage {
                SendStage::BeforeSubmitKey => {
                    self.erase_dropped_probe(at, shown.len(), flight).await
                }
                SendStage::AfterSubmitKey => {
                    Ok(match self.erase_probe(at, shown.len(), flight).await? {
                        ProbeErased::Emptied(..) => declined(NotPlacedReason::Taken, None),
                        ProbeErased::Interrupted(_) => {
                            declined_unvouched(NotPlacedReason::Vanished)
                        }
                        ProbeErased::Undecidable => self.declined_undecidable(),
                    })
                }
            },
            ProbeReading::Shown {
                behind: BehindProbe::Text(front),
                shown,
                at,
                flight,
                ..
            } if envelope_shows(&front) => {
                if let ShownText::Body(before) = &front {
                    self.note_read_back(before, line);
                }
                self.erase_probe_then_resend(line, &front, at, shown.len(), flight, requester)
                    .await
            }
            ProbeReading::Shown {
                behind: before @ BehindProbe::Text(_),
                shown,
                at,
                flight,
                ..
            } => Ok(
                match self
                    .erase_probe_behind(at, shown.len(), flight, &before)
                    .await?
                {
                    ErasedBehind::Undecidable => self.declined_undecidable(),
                    ErasedBehind::Moved | ErasedBehind::Erased => {
                        foreign_draft(declined_unvouched(NotPlacedReason::Foreign))
                    }
                },
            ),
            ProbeReading::Interrupted => Ok(declined_unvouched(NotPlacedReason::Interrupted)),
            ProbeReading::Abandoned => Ok(declined_unvouched(NotPlacedReason::Unobservable)),
            ProbeReading::Undecidable => Ok(self.declined_undecidable()),
        }
    }

    /// Erases `count` probes that showed right behind the caller's own
    /// envelope (ledger at `seq`), and presses the submit key alone once the
    /// composer reads as `target` again.
    async fn erase_probe_then_resend(
        &self,
        line: &ComposerLine,
        target: &ShownText,
        seq: u64,
        count: usize,
        flight: Flight,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        match self
            .erase_probe_back_to(seq, count, flight, target, line, requester)
            .await?
        {
            ErasedBack::Restored { at } => self.resend_key_only(line, at, requester).await,
            ErasedBack::OtherWriter { .. } => Ok(declined_unvouched(NotPlacedReason::Interrupted)),
            ErasedBack::RequesterGone { .. } => {
                Ok(declined_unvouched(NotPlacedReason::Unobservable))
            }
            ErasedBack::Undecidable => Ok(self.declined_undecidable()),
        }
    }

    /// Erases `count` probes that showed behind `before` (ledger at `seq`),
    /// and reads, on a screen that accepts a write, until the erase is
    /// reached -- the composer shows `before` again, or something that no
    /// longer ends with one of `flight`'s probes -- or another writer moves
    /// the ledger, so the next reader sees what was there before the probes.
    /// Probes a dialog kept from being erased are counted behind the next
    /// probe and erased again.
    async fn erase_probe_behind(
        &self,
        seq: u64,
        count: usize,
        flight: Flight,
        before: &BehindProbe,
    ) -> PtyResult<ErasedBehind> {
        let (mut seq, mut count, mut flight) = (seq, count, flight);
        let mut moved = ErasedBehind::Moved;
        loop {
            if self
                .write_judged(&self.erase_keys(count), seq, TakeBackRegistration::Empty)
                .await?
                == GuardedWrite::Moved
            {
                return Ok(moved);
            }
            moved = ErasedBehind::Erased;
            let check = ProbeCheck {
                route: "erase_probe_behind",
                at: seq + 1,
                first: FirstWrite::Written,
                flight,
                avoid: before.end().into_iter().collect(),
                expected: None,
                counted: false,
                unconfirmed: None,
            };
            let reached = |observation: &ComposerObservation, typed: &[char]| {
                // A screen not readable yet is no sign the erase was reached.
                let erased = match &observation.readout {
                    ComposerReadout::Body(text) | ComposerReadout::Tail(text) => {
                        !last_visible(text).is_some_and(|c| typed.contains(&c))
                    }
                    ComposerReadout::Empty => true,
                    ComposerReadout::NotObserved => false,
                };
                (screen_accepts_write(observation)
                    && (erased || before.shows_again_in(&observation.readout)))
                .then(|| (before.clone(), Vec::new()))
            };
            match self
                .confirm_own_probes(
                    check,
                    &mut Requester::none(),
                    |_| TakeBackRegistration::Empty,
                    reached,
                )
                .await?
            {
                ProbeWait::Shown {
                    shown,
                    at,
                    flight: next,
                    ..
                } if !shown.is_empty() => {
                    (seq, count, flight) = (at, shown.len(), next);
                }
                ProbeWait::Shown { .. }
                | ProbeWait::NotWritten
                | ProbeWait::OtherWriter { .. }
                | ProbeWait::RequesterGone { .. } => return Ok(ErasedBehind::Erased),
                ProbeWait::Undecidable => return Ok(ErasedBehind::Undecidable),
            }
        }
    }

    /// Writes a probe behind everything written so far (ledger at `seq`) and
    /// reads until the composer has changed from what `observed` read and
    /// ends with it, on a screen that accepts a write. Input is handled in
    /// order, so a probe that shows means everything before it has been
    /// handled. `in_flight` are probes of an earlier write that may still be
    /// on their way, which this probe differs from. A requester that leaves
    /// first writes no probe, and one that leaves while it is on its way
    /// leaves it recorded as unanswered.
    async fn read_through_probe(
        &self,
        seq: u64,
        observed: &ComposerObservation,
        in_flight: &[char],
        requester: &mut Requester,
    ) -> PtyResult<ProbeReading> {
        if requester.has_left() {
            return Ok(ProbeReading::Abandoned);
        }
        let before_probe = &observed.readout;
        let check = ProbeCheck {
            route: "read_through_probe",
            at: seq,
            first: FirstWrite::Probe,
            flight: Flight {
                typed: in_flight.to_vec(),
                closures: observed.closed_screens,
            },
            avoid: readout_end(before_probe).into_iter().collect(),
            expected: None,
            counted: false,
            unconfirmed: Some(before_probe),
        };
        let shown_before = |observation: &ComposerObservation, typed: &[char]| {
            let probe = *typed.last()?;
            if &observation.readout == before_probe || !screen_accepts_write(observation) {
                return None;
            }
            BehindProbe::of(&observation.readout, probe).map(|behind| (behind, vec![probe]))
        };
        // Nothing before this probe is confirmed as this call's own yet.
        Ok(
            match self
                .confirm_own_probes(
                    check,
                    requester,
                    |_| TakeBackRegistration::Empty,
                    shown_before,
                )
                .await?
            {
                ProbeWait::Shown {
                    behind,
                    shown,
                    at,
                    flight,
                    observation,
                } => ProbeReading::Shown {
                    behind,
                    shown,
                    at,
                    flight,
                    observation,
                },
                ProbeWait::NotWritten | ProbeWait::OtherWriter { .. } => ProbeReading::Interrupted,
                ProbeWait::RequesterGone { .. } => ProbeReading::Abandoned,
                ProbeWait::Undecidable => ProbeReading::Undecidable,
            },
        )
    }

    /// The one wait on the daemon's own probes, for every write it judges by
    /// one and every erase of one: types the first probe when `check` asks
    /// for it, then reads until the probes are read.
    ///
    /// Until a screen the gate closes on has been counted (see
    /// [`ComposerObservation::closed_screens`]), `settle` reads them, given
    /// the probes typed so far. A dialog may have taken a probe or an erase
    /// as its own input, so once one has been counted and the screen accepts
    /// a write again with nothing read, the next probe character is typed
    /// behind (see [`ProbeChars::pick`]). From then on only the last probe
    /// typed showing decides, and the probes are counted back from the end
    /// ([`count_own_probes`]). With no character left to type, it ends
    /// [`ProbeWait::Undecidable`]: the number of probes is the bound, not a
    /// clock. A closed screen that took nothing costs one probe typed and
    /// erased again. Probes it ends without seeing are left through
    /// [`WindowEntry::leave_unseen_probes`].
    ///
    /// # Errors
    ///
    /// [`crate::pty::PtyError::ChildExited`] once the child exits, and what a
    /// probe's write propagates.
    async fn confirm_own_probes(
        &self,
        check: ProbeCheck<'_>,
        requester: &mut Requester,
        registration: impl Fn(&[char]) -> TakeBackRegistration,
        mut settle: impl FnMut(&ComposerObservation, &[char]) -> Option<(BehindProbe, Vec<char>)>,
    ) -> PtyResult<ProbeWait> {
        let ProbeCheck {
            route,
            mut at,
            first,
            mut flight,
            avoid,
            expected,
            mut counted,
            unconfirmed,
        } = check;
        let Some(probes) = self.input.probe.as_ref() else {
            return Ok(ProbeWait::Undecidable);
        };
        self.leave_unseen_probes(&flight, unconfirmed, None);
        if first == FirstWrite::Probe {
            let probe = probes.pick_first(&flight.typed, &avoid);
            if !self
                .type_probe(probe, at, &mut flight, &registration, unconfirmed)
                .await?
            {
                return Ok(ProbeWait::NotWritten);
            }
            at += 1;
        }
        loop {
            let step = {
                let flight = &flight;
                self.wait_for_composer(route, Some(at), requester, |observation| {
                    let decided = if counted {
                        screen_accepts_write(observation)
                            .then(|| {
                                count_own_probes(&observation.readout, &flight.typed, expected)
                            })
                            .flatten()
                    } else {
                        settle(observation, &flight.typed)
                    };
                    probe_step(observation, probes, flight, &avoid, decided)
                        .map(|step| (step, observation.clone()))
                })
                .await
            };
            match step {
                ComposerWait::Done((ProbeStep::Decided(behind, shown), observation)) => {
                    // What follows (the erase of these probes) is decided on
                    // this reading, so only a screen closed after it counts.
                    flight.closures = observation.closed_screens;
                    return Ok(ProbeWait::Shown {
                        behind,
                        shown,
                        at,
                        flight,
                        observation,
                    });
                }
                ComposerWait::Done((ProbeStep::Retype(probe, closures), _)) => {
                    if !self
                        .type_probe(probe, at, &mut flight, &registration, unconfirmed)
                        .await?
                    {
                        return Ok(ProbeWait::OtherWriter { at });
                    }
                    at += 1;
                    flight.closures = closures;
                    counted = true;
                    tracing::debug!(
                        window_id = %self.window_id(),
                        route,
                        typed = flight.typed.len(),
                        "a closed screen may have taken a probe; typed the next one"
                    );
                }
                ComposerWait::Done((ProbeStep::Undecidable, _)) => {
                    tracing::info!(
                        window_id = %self.window_id(),
                        route,
                        typed = flight.typed.len(),
                        "a closed screen may have taken every probe; declining the write"
                    );
                    return Ok(ProbeWait::Undecidable);
                }
                ComposerWait::OtherWriter => return Ok(ProbeWait::OtherWriter { at }),
                ComposerWait::RequesterGone => {
                    self.leave_unseen_probes(&flight, unconfirmed, Some(at));
                    return Ok(ProbeWait::RequesterGone { at, flight });
                }
                ComposerWait::Exited => return Err(crate::pty::PtyError::ChildExited),
            }
        }
    }

    /// Writes `probe` at ledger `at` as the next of `flight`'s probes, and
    /// leaves them as unconfirmed behind `unconfirmed` when given.
    /// `false` when another writer wrote first, and nothing was written.
    async fn type_probe(
        &self,
        probe: char,
        at: u64,
        flight: &mut Flight,
        registration: &impl Fn(&[char]) -> TakeBackRegistration,
        unconfirmed: Option<&ComposerReadout>,
    ) -> PtyResult<bool> {
        let mut typed = flight.typed.clone();
        typed.push(probe);
        let mut bytes = [0u8; 4];
        if self
            .write_judged(
                probe.encode_utf8(&mut bytes).as_bytes(),
                at,
                registration(&typed),
            )
            .await?
            == GuardedWrite::Moved
        {
            return Ok(false);
        }
        flight.typed = typed;
        self.leave_unseen_probes(flight, unconfirmed, None);
        Ok(true)
    }

    /// The one place probes a check has typed and not seen counted are left:
    /// how many of them are in the composer is never taken from how many were
    /// typed, since a dialog may have taken any of them, but counted back
    /// from the end of a later reading ([`probes_behind`]). Typed behind
    /// `unconfirmed`, they are the call's own to check once another writer
    /// ends it ([`WindowEntry::check_unconfirmed_probe`]); left by a
    /// requester at ledger count `unanswered_at`, the next call settles them
    /// while nothing is written since.
    fn leave_unseen_probes(
        &self,
        flight: &Flight,
        unconfirmed: Option<&ComposerReadout>,
        unanswered_at: Option<u64>,
    ) {
        if flight.typed.is_empty() {
            return;
        }
        if let Some(before) = unconfirmed {
            lock_recover(&self.own_part).left = OwnLeft::UnconfirmedProbe {
                before: before.clone(),
                probes: flight.typed.clone(),
            };
        }
        if let Some(at) = unanswered_at {
            *lock_recover(&self.unanswered_probe) =
                Some((at, flight.typed.clone(), flight.closures));
        }
    }

    /// The erase key, `count` times over, as one write.
    fn erase_keys(&self, count: usize) -> Vec<u8> {
        self.input.erase.repeat(count)
    }

    /// Whether the write ledger alone vouches for
    /// re-sending `residue` -- the caller brought the same text it wants to
    /// write, and not one write has landed in this window since the count it
    /// brought with it. Every write, from every writer, moves that count, so
    /// a match means the composer holds what this caller left there and
    /// nothing else; another writer's bytes cannot pass as the caller's own
    /// by being identical. It never stands in for the screen: the envelope,
    /// or a probe written behind it, still has to show before the submit key
    /// is pressed.
    fn record_vouches_for_residue(
        &self,
        residue: Option<LineResidue<'_>>,
        line: &ComposerLine,
    ) -> bool {
        residue.is_some_and(|residue| residue.line == line && residue.write_seq == self.write_seq())
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
        line: &ComposerLine,
        seq: u64,
        requester: &Requester,
    ) -> PtyResult<SubmitJudgment> {
        if requester.has_left() {
            return Ok(declined(NotPlacedReason::Unobservable, Some(seq)));
        }
        let plan = plan_submit_line(&self.input, line, SubmitAction::ResendOnly);
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

    /// Writes `line` fresh into a composer just observed empty, then the
    /// probe behind it, and decides from what the composer shows once the
    /// probe does (see [`reflection_of`]): the body landed, so the probe is
    /// erased and the submit key pressed once the composer reads as it did
    /// before the probe; the body was dropped, so the probe is erased and
    /// nothing is left; or nothing shows yet, so the call waits.
    /// `observed` is the reading that showed the composer empty, so the write
    /// gate is checked before this call's very first byte rather than against
    /// a fresh reading of its own; `seq` is the ledger that empty reading was
    /// taken at.
    ///
    /// The probe is typed only on a screen that accepts a write: a dialog
    /// covering the composer once the body is written is waited out first,
    /// never declined on. A dialog that comes up after the probe is waited
    /// out too, and the next probe character is typed behind it when nothing
    /// shows (see [`WindowEntry::confirm_own_probes`]). A write from another
    /// writer ends the call with the body reported as remaining: the body is
    /// registered as this call's own, so that writer's bytes go out behind
    /// its take-back, and nothing more of this plan is written after them
    /// ([`WindowEntry::submit_message`] then checks what is left). A
    /// requester that leaves once the body is written gets no submit key;
    /// the body is taken back instead (see
    /// [`WindowEntry::take_back_for_departed_requester`]).
    async fn write_fresh_and_decide(
        &self,
        line: &ComposerLine,
        observed: &ComposerObservation,
        seq: u64,
        stage: SendStage,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        if !screen_accepts_write(observed) {
            return Ok(decline_not_accepting(None));
        }
        // `judge_after_submit_key` settles an empty composer after the key;
        // this keeps any other route from writing the body a second time.
        if stage == SendStage::AfterSubmitKey {
            return Ok(declined(NotPlacedReason::Vanished, None));
        }
        if requester.has_left() {
            return Ok(declined(NotPlacedReason::Unobservable, None));
        }

        let plan = plan_submit_line(&self.input, line, SubmitAction::Write);
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
            .write_judged(&body_step.bytes, seq, self.own_text(line.as_str()))
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
                .take_back_for_departed_requester(line.as_str(), after_body)
                .await;
        }
        // Closed screens counted before this reading are not taken for one
        // that took the probe.
        let accepting = |observation: &ComposerObservation| {
            (screen_accepts_write(observation)
                && observation.readout != ComposerReadout::NotObserved)
                .then(|| observation.clone())
        };
        let ahead = match self
            .wait_for_composer(
                "write_fresh_and_decide",
                Some(after_body),
                requester,
                accepting,
            )
            .await
        {
            ComposerWait::Done(observation) => observation,
            ComposerWait::OtherWriter => {
                return Ok(declined(NotPlacedReason::Interrupted, Some(after_body)));
            }
            ComposerWait::RequesterGone => {
                return self
                    .take_back_for_departed_requester(line.as_str(), after_body)
                    .await;
            }
            ComposerWait::Exited => return Err(crate::pty::PtyError::ChildExited),
        };
        let check = ProbeCheck {
            route: "wait_for_reflection",
            at: after_body,
            first: FirstWrite::Probe,
            flight: Flight {
                typed: Vec::new(),
                closures: ahead.closed_screens,
            },
            avoid: last_visible(line.as_str())
                .into_iter()
                .chain(readout_end(&ahead.readout))
                .collect(),
            expected: Some(line),
            counted: false,
            unconfirmed: None,
        };
        match self.wait_for_reflection(line, check, requester).await? {
            ProbeWait::Shown {
                behind: BehindProbe::Text(front),
                shown,
                at,
                flight,
                ..
            } => {
                self.erase_probe_then_submit(
                    line,
                    &front,
                    &submit_step,
                    at,
                    shown.len(),
                    flight,
                    requester,
                )
                .await
            }
            ProbeWait::Shown {
                behind: BehindProbe::Alone,
                shown,
                at,
                flight,
                ..
            } => self.erase_dropped_probe(at, shown.len(), flight).await,
            ProbeWait::NotWritten => Ok(declined(NotPlacedReason::Interrupted, Some(after_body))),
            ProbeWait::OtherWriter { at } => Ok(declined(NotPlacedReason::Interrupted, Some(at))),
            ProbeWait::RequesterGone { at, flight } => {
                self.take_back_for_departed_requester(&line.with_probes(&flight.typed), at)
                    .await
            }
            ProbeWait::Undecidable => Ok(self.declined_undecidable()),
        }
    }

    /// [`WindowEntry::confirm_own_probes`] for what a fresh `line` and the
    /// probes behind it leave in the composer, on a screen that accepts a
    /// write.
    async fn wait_for_reflection(
        &self,
        line: &ComposerLine,
        check: ProbeCheck<'_>,
        requester: &mut Requester,
    ) -> PtyResult<ProbeWait> {
        let started = Instant::now();
        let reflection = self
            .confirm_own_probes(
                check,
                requester,
                |typed| self.own_text(&line.with_probes(typed)),
                reflection_settle(line),
            )
            .await?;
        let (outcome, reflected) = match &reflection {
            ProbeWait::Shown {
                behind: BehindProbe::Text(ShownText::Body(before)),
                ..
            } => {
                self.note_read_back(before, line);
                (
                    "behind",
                    composer_text_matches_ignoring_lost_whitespace(before, line.as_str()),
                )
            }
            ProbeWait::Shown {
                behind: BehindProbe::Alone,
                ..
            } => ("dropped", false),
            ProbeWait::Shown {
                behind: BehindProbe::Text(ShownText::Tail(before)),
                ..
            } => ("behind_tail", tail_is_end_of(before, line.as_str())),
            ProbeWait::NotWritten | ProbeWait::OtherWriter { .. } => ("other_writer", false),
            ProbeWait::RequesterGone { .. } => ("requester_gone", false),
            ProbeWait::Undecidable => ("undecidable", false),
        };
        // Whether what shows before the probe matches the body this call
        // wrote is not part of the decision, but is worth seeing when
        // debugging a delivery.
        tracing::debug!(
            window_id = %self.window_id(),
            body_bytes = line.as_str().len(),
            elapsed_ms = started.elapsed().as_millis(),
            outcome,
            reflected,
            "composer reflection observed"
        );
        Ok(reflection)
    }

    /// Erases `count` probes that showed behind the composer's text (ledger
    /// at `seq`) and waits until the composer reads as `target` again, on a
    /// screen that accepts a write. A reading that cannot be read is waited
    /// on, not taken for the erase having been reached. `own` is this call's
    /// text the composer holds once the probes are erased. Probes a dialog
    /// kept from being erased are counted behind the next probe and erased
    /// again.
    async fn erase_probe_back_to(
        &self,
        seq: u64,
        count: usize,
        flight: Flight,
        target: &ShownText,
        own: &ComposerLine,
        requester: &mut Requester,
    ) -> PtyResult<ErasedBack> {
        let (mut seq, mut count, mut flight) = (seq, count, flight);
        loop {
            if self
                .write_judged(&self.erase_keys(count), seq, self.own_text(own.as_str()))
                .await?
                == GuardedWrite::Moved
            {
                return Ok(ErasedBack::OtherWriter { at: seq });
            }
            let check = ProbeCheck {
                route: "erase_probe_back_to",
                at: seq + 1,
                first: FirstWrite::Written,
                flight,
                avoid: target.end().into_iter().collect(),
                expected: None,
                counted: false,
                unconfirmed: None,
            };
            let reads_as_before = |observation: &ComposerObservation, _: &[char]| {
                (screen_accepts_write(observation) && target.restored_in(&observation.readout, own))
                    .then(|| (BehindProbe::Text(target.clone()), Vec::new()))
            };
            match self
                .confirm_own_probes(
                    check,
                    requester,
                    |typed| self.own_text(&own.with_probes(typed)),
                    reads_as_before,
                )
                .await?
            {
                ProbeWait::Shown { shown, at, .. } if shown.is_empty() => {
                    return Ok(ErasedBack::Restored { at });
                }
                ProbeWait::Shown {
                    shown,
                    at,
                    flight: next,
                    ..
                } => (seq, count, flight) = (at, shown.len(), next),
                ProbeWait::NotWritten => return Ok(ErasedBack::OtherWriter { at: seq + 1 }),
                ProbeWait::OtherWriter { at } => return Ok(ErasedBack::OtherWriter { at }),
                ProbeWait::RequesterGone { at, flight } => {
                    return Ok(ErasedBack::RequesterGone { at, flight });
                }
                ProbeWait::Undecidable => return Ok(ErasedBack::Undecidable),
            }
        }
    }

    /// The body landed and `count` probes show behind it (ledger at `seq`).
    /// Erases them and presses the submit key once the composer reads as
    /// `target` again.
    // The probes to erase are part of what the caller found.
    #[allow(clippy::too_many_arguments)]
    async fn erase_probe_then_submit(
        &self,
        line: &ComposerLine,
        target: &ShownText,
        submit_step: &SubmitStep,
        seq: u64,
        count: usize,
        flight: Flight,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        if requester.has_left() {
            return self
                .take_back_for_departed_requester(&line.with_probes(&flight.typed), seq)
                .await;
        }
        match self
            .erase_probe_back_to(seq, count, flight, target, line, requester)
            .await?
        {
            ErasedBack::Restored { at } => self.press_submit_key(submit_step, at).await,
            ErasedBack::OtherWriter { at } => Ok(declined(NotPlacedReason::Interrupted, Some(at))),
            ErasedBack::RequesterGone { at, flight } => {
                self.take_back_for_departed_requester(&line.with_probes(&flight.typed), at)
                    .await
            }
            ErasedBack::Undecidable => Ok(self.declined_undecidable()),
        }
    }

    /// The receiver dropped the body and shows `count` probes alone (ledger
    /// at `seq`): erases them and declines once the composer reads empty,
    /// nothing of this call's left behind.
    async fn erase_dropped_probe(
        &self,
        seq: u64,
        count: usize,
        flight: Flight,
    ) -> PtyResult<SubmitJudgment> {
        match self.erase_probe(seq, count, flight).await? {
            ProbeErased::Emptied(..) => Ok(declined(NotPlacedReason::NotReflected, None)),
            ProbeErased::Interrupted(_) => Ok(declined_unvouched(NotPlacedReason::Interrupted)),
            ProbeErased::Undecidable => Ok(self.declined_undecidable()),
        }
    }

    /// The requester left once this call had written `written` (its line and
    /// the probes typed behind it) into the composer, leaving the ledger at
    /// `seq`. Takes it back and answers once the composer is confirmed empty,
    /// however long the receiver takes, holding the window's turn meanwhile
    /// so nothing is written on top. Without a way to take it back, the body
    /// is reported as remaining and the degenerate-empty marker stops a later
    /// attempt from writing it a second time.
    async fn take_back_for_departed_requester(
        &self,
        written: &str,
        seq: u64,
    ) -> PtyResult<SubmitJudgment> {
        if self.input.take_back() == TakeBack::Unavailable {
            *lock_recover(&self.degenerate_since) = Some(self.write_seq());
            return Ok(declined_unvouched(NotPlacedReason::NotReflected));
        }
        // The take-back is a write of its own, so it waits for a screen that
        // accepts one, the same as every other write.
        let Some(before_take_back) = self
            .wait_for_composer_unwatched("take_back_for_departed_requester", seq, |observation| {
                (screen_accepts_write(observation)
                    && !matches!(observation.readout, ComposerReadout::NotObserved))
                .then(|| observation.clone())
            })
            .await?
        else {
            return Ok(declined(NotPlacedReason::Interrupted, Some(seq)));
        };
        match self
            .take_back_and_settle(written, seq, &before_take_back)
            .await?
        {
            TakenBack::Emptied(..) => Ok(declined(NotPlacedReason::NotReflected, None)),
            TakenBack::Declined(judgment) => Ok(judgment),
        }
    }

    /// The composer holds a leftover this caller's own record vouches for
    /// (`residue`), but the text it wants to send now differs from it. Takes
    /// the leftover back and, once the composer is confirmed empty, writes
    /// `line` as a fresh message; any other outcome leaves the leftover
    /// reported as remaining.
    async fn take_back_own_residue_then_write(
        &self,
        line: &ComposerLine,
        residue: LineResidue<'_>,
        observed: &ComposerObservation,
        stage: SendStage,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        if self.input.take_back() == TakeBack::Unavailable {
            return Ok(foreign_draft(declined(NotPlacedReason::Foreign, None)));
        }
        if !screen_accepts_write(observed) {
            return Ok(decline_not_accepting(Some(residue.write_seq)));
        }
        if requester.has_left() {
            return Ok(declined(
                NotPlacedReason::Unobservable,
                Some(residue.write_seq),
            ));
        }
        match self
            .take_back_and_settle(residue.line.as_str(), residue.write_seq, observed)
            .await?
        {
            TakenBack::Emptied(settled, seq) => {
                self.write_fresh_and_decide(line, &settled, seq, stage, requester)
                    .await
            }
            TakenBack::Declined(judgment) => Ok(judgment),
        }
    }

    /// Writes the take-back of `written` (whose write left the ledger at
    /// `seq_after_written`, and the composer reading as `before_take_back`),
    /// then a probe, and settles what the composer holds once the probe
    /// shows (see [`WindowEntry::settle_with_probe`]). A foreign write since
    /// is a declined judgment that is never taken back.
    async fn take_back_and_settle(
        &self,
        written: &str,
        seq_after_written: u64,
        before_take_back: &ComposerObservation,
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
        self.settle_with_probe(after_take_back, before_take_back, written)
            .await
    }

    /// Writes a probe behind a take-back (ledger at `seq`) and reads until
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
        seq: u64,
        before_take_back: &ComposerObservation,
        written: &str,
    ) -> PtyResult<TakenBack> {
        let before = &before_take_back.readout;
        let check = ProbeCheck {
            route: "settle_with_probe",
            at: seq,
            first: FirstWrite::Probe,
            flight: Flight {
                typed: Vec::new(),
                closures: before_take_back.closed_screens,
            },
            avoid: readout_end(before)
                .into_iter()
                .chain(last_visible(split_own_probes(written).0))
                .collect(),
            expected: None,
            counted: false,
            unconfirmed: None,
        };
        let probe_shown = |observation: &ComposerObservation, typed: &[char]| {
            let probe = *typed.last()?;
            match &observation.readout {
                ComposerReadout::Body(text)
                    if &observation.readout != before
                        && screen_accepts_write(observation)
                        && !reads_as_before_take_back(text, written) =>
                {
                    let front = text.strip_suffix(probe)?;
                    let behind = if front.is_empty() {
                        BehindProbe::Alone
                    } else {
                        BehindProbe::Text(ShownText::Body(front.to_string()))
                    };
                    Some((behind, vec![probe]))
                }
                ComposerReadout::Body(_)
                | ComposerReadout::Empty
                | ComposerReadout::NotObserved
                | ComposerReadout::Tail(_) => None,
            }
        };
        let registration = |typed: &[char]| {
            let mut text = written.to_string();
            text.extend(typed);
            self.own_text(&text)
        };
        match self
            .confirm_own_probes(check, &mut Requester::none(), registration, probe_shown)
            .await?
        {
            ProbeWait::Shown {
                behind: BehindProbe::Alone,
                shown,
                at,
                flight,
                ..
            } => {
                let erased = self.erase_probe(at, shown.len(), flight).await?;
                Ok(self.taken_back_once(erased))
            }
            ProbeWait::Shown {
                behind: BehindProbe::Text(_),
                shown,
                at,
                ..
            } => {
                // Input is handled in order, so this erases the probes behind
                // whatever the take-back left.
                match self
                    .write_judged(&self.erase_keys(shown.len()), at, self.own_text(written))
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
            ProbeWait::NotWritten => Ok(TakenBack::Declined(declined(
                NotPlacedReason::Interrupted,
                Some(seq),
            ))),
            ProbeWait::OtherWriter { .. } | ProbeWait::RequesterGone { .. } => Ok(
                TakenBack::Declined(declined_unvouched(NotPlacedReason::Interrupted)),
            ),
            ProbeWait::Undecidable => Ok(TakenBack::Declined(self.declined_undecidable())),
        }
    }

    /// A take-back's outcome once its probes have been erased.
    fn taken_back_once(&self, erased: ProbeErased) -> TakenBack {
        match erased {
            ProbeErased::Emptied(settled, seq) => TakenBack::Emptied(settled, seq),
            ProbeErased::Interrupted(after_erase) => {
                TakenBack::Declined(declined(NotPlacedReason::Interrupted, Some(after_erase)))
            }
            ProbeErased::Undecidable => TakenBack::Declined(self.declined_undecidable()),
        }
    }

    /// Erases `count` probes that show alone in the composer, with the ledger
    /// at `seq` and nothing else written since, and reads until the composer
    /// is empty on a screen that accepts a write, or another writer moves the
    /// ledger. Probes a dialog kept from being erased are counted behind the
    /// next probe and erased again. Erasing its own probes is cleanup, so no
    /// requester stops it.
    async fn erase_probe(&self, seq: u64, count: usize, flight: Flight) -> PtyResult<ProbeErased> {
        let (mut seq, mut count, mut flight) = (seq, count, flight);
        loop {
            if self
                .write_judged(&self.erase_keys(count), seq, TakeBackRegistration::Empty)
                .await?
                == GuardedWrite::Moved
            {
                return Ok(ProbeErased::Interrupted(seq));
            }
            let check = ProbeCheck {
                route: "erase_probe",
                at: seq + 1,
                first: FirstWrite::Written,
                flight,
                avoid: Vec::new(),
                expected: None,
                counted: false,
                unconfirmed: None,
            };
            let emptied = |observation: &ComposerObservation, _: &[char]| {
                (matches!(observation.readout, ComposerReadout::Empty)
                    && screen_accepts_write(observation))
                .then(|| (BehindProbe::Alone, Vec::new()))
            };
            match self
                .confirm_own_probes(
                    check,
                    &mut Requester::none(),
                    |_| TakeBackRegistration::Empty,
                    emptied,
                )
                .await?
            {
                ProbeWait::Shown {
                    behind: BehindProbe::Alone,
                    shown,
                    at,
                    observation,
                    ..
                } if shown.is_empty() => {
                    *lock_recover(&self.unanswered_probe) = None;
                    *lock_recover(&self.degenerate_since) = None;
                    return Ok(ProbeErased::Emptied(observation, at));
                }
                ProbeWait::Shown {
                    behind: BehindProbe::Alone,
                    shown,
                    at,
                    flight: next,
                    ..
                } => (seq, count, flight) = (at, shown.len(), next),
                ProbeWait::Shown {
                    behind: BehindProbe::Text(_),
                    ..
                }
                | ProbeWait::Undecidable => return Ok(ProbeErased::Undecidable),
                ProbeWait::NotWritten => return Ok(ProbeErased::Interrupted(seq + 1)),
                ProbeWait::OtherWriter { at } | ProbeWait::RequesterGone { at, .. } => {
                    return Ok(ProbeErased::Interrupted(at));
                }
            }
        }
    }

    /// Settles probes an earlier call left unanswered, when the composer now
    /// holds them alone -- the last one typed showing -- on a screen that
    /// accepts a write and nothing was written since: whatever was written
    /// before them has been handled and nothing else remains, so they are
    /// erased and the window may be written to again.
    async fn settle_unanswered_probe(
        &self,
        text: &str,
        observed: &ComposerObservation,
    ) -> PtyResult<Option<TakenBack>> {
        let Some((seq, probes, _)) = lock_recover(&self.unanswered_probe).clone() else {
            return Ok(None);
        };
        if !screen_accepts_write(observed) || self.write_seq() != seq {
            return Ok(None);
        }
        let Some((BehindProbe::Alone, shown)) =
            count_own_probes(&ComposerReadout::Body(text.to_string()), &probes, None)
        else {
            return Ok(None);
        };
        let flight = Flight {
            typed: probes,
            closures: observed.closed_screens,
        };
        let erased = self.erase_probe(seq, shown.len(), flight).await?;
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

    /// The decline for [`ProbeWait::Undecidable`] on every route: where the
    /// write went cannot be told from the screen, so it is handled as a
    /// take-back that could not be confirmed.
    fn declined_undecidable(&self) -> SubmitJudgment {
        self.decline_unconfirmed_take_back(NotPlacedReason::NotAccepting)
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
    /// The token this returns `true` for is the one handed to this window's
    /// own hooks and bridge (in its credentials file, or in its child
    /// environment for a profile in `env` mode), so a process holding it can
    /// only speak for itself; the value never authenticates a
    /// request aimed at a different window.
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
        self.status_token
            .get()
            .is_some_and(|token| token.verify(candidate))
    }

    /// The parcels waiting for this window's CLI.
    pub fn inbox(&self) -> &Inbox {
        &self.inbox
    }

    /// The `[delivery]` table of the profile this window was launched from.
    #[must_use]
    pub fn delivery(&self) -> Option<&ProfileDelivery> {
        self.provenance.delivery.as_ref()
    }

    /// Whether what the daemon sends of its own accord can reach this
    /// window's CLI: its profile declares delivery, and the hooks the daemon
    /// arranged were not left out at launch.
    #[must_use]
    pub fn delivers(&self) -> bool {
        takes_delivery(self.provenance.delivery.is_some(), self.provenance.hooks)
    }

    /// The program a wake by command runs, when the profile declares one.
    #[must_use]
    pub fn wake_command(&self) -> Option<&WakeCommand> {
        self.provenance.wake_command.as_ref()
    }

    /// The session identifier the window's CLI reported, if it has.
    #[must_use]
    pub fn session_id(&self) -> Option<String> {
        lock_recover(&self.session_id).clone()
    }

    /// Keeps `candidate` as the window's session identifier when it has the
    /// shape of one -- 1 to 128 ASCII letters, digits and `-_.:`, not starting
    /// with `-` -- and returns whether it did. Anything else is refused and
    /// the held identifier stays: the value ends up in command lines and
    /// must never be able to carry an option, a path or a shell word.
    pub fn record_session_id(&self, candidate: &str) -> bool {
        if !is_session_id(candidate) {
            return false;
        }
        *lock_recover(&self.session_id) = Some(candidate.to_string());
        self.update_row();
        // A wake by command needs the session, so a parcel held for it can go now.
        self.notice_wake.notify_one();
        true
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

    /// Whether this window currently has a detector attached: `false` once
    /// [`WindowEntry::confirm_exited`] has taken it, which happens before
    /// `Exited` is published.
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
    /// is not seen here; the status forwarder reports that window as
    /// `unknown` instead, until its exit is confirmed.
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
        self.status_gate.subscribe()
    }

    /// The current confirmed status together with the instant it became the
    /// confirmed status, read as one value so the two can never disagree.
    pub fn status_snapshot(&self) -> StatusAt {
        self.status_gate.snapshot()
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
        self.status_gate.watch()
    }

    /// What this window's *current* published pair says about its startup
    /// phase, latching if the pair already proves it. A caller that gets
    /// [`StartupEvidence::Building`] may wait for the hold to complete; a
    /// caller that gets [`StartupEvidence::Absent`] has nothing to wait for.
    pub fn startup_evidence(&self) -> StartupEvidence {
        self.status_gate.startup_evidence()
    }

    /// Records what a completed hold wait proved, so the next handoff into
    /// this window does not wait again.
    pub fn note_left_startup_phase(&self) {
        self.status_gate.note_left_startup_phase();
    }

    /// Publishes a status the detector confirmed. A confirmed `Exited` goes
    /// through [`WindowEntry::confirm_exited`], like every other `Exited`.
    fn publish_confirmed(&self, confirmed: ConfirmedStatus) {
        if confirmed.status == DetectedStatus::Exited {
            let _ = self.confirm_exited();
        } else {
            self.status_gate.publish(&self.window_id, confirmed);
        }
    }

    /// Fails this window's status back to `Unknown(DetectorStopped)` once its
    /// detection stopped reporting, unless it has already exited.
    fn publish_detector_loss(&self) {
        self.status_gate.publish_loss(&self.window_id);
    }

    /// Waits, at most `grace`, until the window's holder has ended, then
    /// publishes an observed exit as [`WindowEntry::confirm_exited`] does. The
    /// exit is read only after the wait, so a close that ended the child
    /// reports it.
    ///
    /// # Errors
    ///
    /// Returns [`ExitUnobserved`] when the session has not reported the
    /// child's exit by then.
    pub(crate) async fn settle_close(
        &self,
        grace: Duration,
    ) -> Result<ExitConfirmed, ExitUnobserved> {
        if !self.session.wait_until_released(grace).await {
            tracing::warn!(
                window_id = self.window_id.as_str(),
                "the window's holder had not ended within the close grace"
            );
        }
        self.confirm_exited()
    }

    /// Publishes `Exited` if the child's exit has been observed, and returns
    /// the proof of it; publishes nothing otherwise. Idempotent: a window
    /// that already reports `Exited` is left as it is and still yields
    /// [`ExitConfirmed`].
    ///
    /// The detector is taken before `Exited` is published, so a reader that
    /// has seen `Exited` never finds one.
    ///
    /// # Errors
    ///
    /// Returns [`ExitUnobserved`] while the session has not reported the
    /// child's exit.
    fn confirm_exited(&self) -> Result<ExitConfirmed, ExitUnobserved> {
        if self.session.exit_status().is_none() {
            return Err(ExitUnobserved { _private: () });
        }
        let detector = lock_recover(&self.detector).take();
        drop(detector);
        let published = self.status_gate.publish(
            &self.window_id,
            ConfirmedStatus {
                status: DetectedStatus::Exited,
                evidence_since: tokio::time::Instant::now(),
            },
        );
        if published == Publication::BecameExited {
            self.participation.raise();
        }
        Ok(ExitConfirmed { _private: () })
    }

    /// Registers this caller as the (new) attach holder, superseding any
    /// previous holder, whose receiver then resolves. The attach lives in
    /// the window's session, with its view (see [`PtySession::show_view`]).
    pub async fn register_attach(&self) -> (AttachGeneration, oneshot::Receiver<()>) {
        self.session.attach_view()
    }

    /// Clears the attach slot if it is still held by `generation` (a stale
    /// disconnect/reattach must not clobber a newer holder).
    pub async fn clear_attach_if(&self, generation: AttachGeneration) {
        self.session.detach_view(generation);
    }

    /// Whether some connection currently holds this window's attach slot.
    /// Used by [`WindowRegistry`] to decide which `Exited` entries are safe
    /// to evict.
    fn is_attached(&self) -> bool {
        self.session.is_view_attached()
    }

    /// This window's current display name.
    pub fn name(&self) -> String {
        lock_recover(&self.name).clone()
    }

    /// Stores `name` and returns the value as stored. Reached only through
    /// [`WindowRegistry::rename_window`], which has already judged the name.
    fn store_name(&self, name: &str) -> String {
        let stored = {
            let mut stored = lock_recover(&self.name);
            name.clone_into(&mut stored);
            stored.clone()
        };
        self.participation.raise();
        self.update_row();
        stored
    }

    /// Replaces the presentation layout this window carries. The value is
    /// stored verbatim; validating it (including its size) is the caller's
    /// responsibility.
    fn set_layout(&self, layout_json: String) {
        *lock_recover(&self.layout_json) = Some(layout_json);
        self.update_row();
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

/// Proof that a window's child exit was observed and its confirmed status
/// is `Exited`. Only the window itself makes one, as it
/// publishes or checks `Exited`.
#[derive(Debug)]
pub struct ExitConfirmed {
    _private: (),
}

/// The child's exit had not been observed, so `Exited` was not published;
/// the exit watcher publishes it once the exit is reported. Only the window
/// itself makes one.
#[derive(Debug)]
pub struct ExitUnobserved {
    _private: (),
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

/// A generation counter raised whenever the set of windows, or a value a chat
/// roster shows, may have changed. It says only that something may have
/// changed; who takes part in a room is decided elsewhere.
pub(crate) struct ParticipationSignal(watch::Sender<u64>);

impl Default for ParticipationSignal {
    fn default() -> Self {
        Self(watch::channel(0).0)
    }
}

impl ParticipationSignal {
    /// Raises the generation; raises that land before a watcher wakes
    /// collapse into one wake-up.
    pub(crate) fn raise(&self) {
        self.0
            .send_modify(|generation| *generation = generation.saturating_add(1));
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
    /// What every window's holder is started in.
    holder_host: Arc<HolderHost>,
    /// The table every window's row goes in, installed once at start-up by
    /// `store::wire_into`; `None` when the registry has no database.
    window_table: Mutex<Option<WindowTable>>,
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
    /// Set once every window is being terminated with the daemon: what their
    /// inboxes still hold is left for the next daemon to settle.
    shutting_down: AtomicBool,
    /// Raised on registration, on a window reaching `Exited` and on a rename.
    participation: Arc<ParticipationSignal>,
    /// The palettes a window takes when it opens: see
    /// [`WindowRegistry::set_default_palettes`].
    default_palettes: Mutex<DefaultPalettes>,
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
        let holder_host = HolderHost::from_config(&config);

        Self {
            config,
            entries: Mutex::new(Vec::new()),
            open_lock: AsyncMutex::new(()),
            holder_host,
            window_table: Mutex::new(None),
            status_token_override,
            store_sender: Mutex::new(None),
            window_opened_hook: Mutex::new(None),
            notice_launcher: Mutex::new(None),
            workspace_walks,
            mcp_config_write: AsyncMutex::new(()),
            opening: Mutex::new(HashMap::new()),
            shutting_down: AtomicBool::new(false),
            participation: Arc::default(),
            default_palettes: Mutex::new(DefaultPalettes {
                app: yagura_protocol::built_in_dark_palette(),
                vendors: BTreeMap::new(),
            }),
        }
    }

    /// Replaces, as a whole, the palettes a window opened from now on answers
    /// its color queries with until a view sends it its own: the one of its
    /// vendor, else `app`. Windows already open keep theirs. Returns whether
    /// the palettes changed.
    pub fn set_default_palettes(&self, palettes: DefaultPalettes) -> bool {
        let mut current = lock_recover(&self.default_palettes);
        if *current == palettes {
            return false;
        }
        *current = palettes;
        true
    }

    /// The palettes [`WindowRegistry::set_default_palettes`] last set; the
    /// built-in dark palette for every window before that.
    #[must_use]
    pub fn default_palettes(&self) -> DefaultPalettes {
        lock_recover(&self.default_palettes).clone()
    }

    fn default_palette_for(&self, vendor: Option<&str>) -> Palette {
        let palettes = lock_recover(&self.default_palettes);
        vendor
            .and_then(|vendor| palettes.vendors.get(vendor))
            .unwrap_or(&palettes.app)
            .clone()
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
            on_name_taken,
            vendor,
            detection,
        } = req;
        let name_supplied = name.is_some();

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
        let on_name_taken = if name_supplied {
            on_name_taken
        } else {
            OnNameTaken::Number
        };

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

        // Owns the window's credentials folder from here on: every return
        // below drops it, and a spawned window hands it to its exit watcher.
        let credentials_folder = self
            .create_credentials_folder(provenance.credentials, provenance.credentials_dir.clone())
            .await;

        // Written outside `open_lock`, after the early rejection: a request
        // over the limit never touches the disk, and one slow write never
        // stalls another connection's open.
        let mut prepared = Vec::with_capacity(injections.len());
        for injection in injections {
            // Their settings live in the credentials folder, which this
            // window does not have.
            if provenance.credentials == CredentialsMode::Launch
                && credentials_folder.is_none()
                && matches!(injection.kind, InjectionKind::Hooks | InjectionKind::Mcp)
            {
                continue;
            }
            prepared.push(self.prepare_injection(injection).await);
        }

        // Refused before the id is reserved, so a refused request starts no open.
        let resolution = self.resolve_window_program(&command)?;

        // The binding decision: the limits and the name are decided under the
        // lock every open and rename takes, and the open in progress holds its
        // seat and its name from here on, so the lock is not held while the
        // window's holder starts.
        let open_guard = self.open_lock.lock().await;
        self.enforce_limits_before_open()?;
        let name = {
            let entries = lock_recover(&self.entries);
            let opening = lock_recover(&self.opening);
            claim_name(&entries, &opening, &name, on_name_taken)?
        };

        // The id and token exist before the spawn, so they can be injected
        // into the child environment.
        let values = window_values(
            &provenance,
            &name,
            &cwd_string,
            vendor.as_ref(),
            &command,
            detection.as_ref(),
        );
        let (window_id, row) = self.reserve_window_id(&values).await?;
        let status_token = self.acquire_status_token();
        let opening = self.begin_opening(&window_id, &name);
        drop(open_guard);

        let palette = self.default_palette_for(vendor.as_deref());
        let record = launch_record(&window_id, status_token.as_hex(), &values);
        let lost = row.lost_notice();
        let ((session, hooks_injected), row) = row
            .open(self.spawn_pty_for_window(
                &command,
                &resolved_cwd,
                &status_token,
                &window_id,
                (record, lost),
                &resolution,
                extra_profile_env,
                &prepared,
                palette,
                provenance.credentials,
                credentials_folder.as_ref(),
            ))
            .await?;
        let session = Arc::new(session);
        if let Some(folder) = credentials_folder {
            remove_credentials_when_exited(&session, folder);
        }

        let DetectionSetup {
            handle: detector_handle,
            detected_rx,
            warning: detection_warning,
            input,
            definition_name,
            composer_access,
            abandons_calls_at_turn_end,
        } = self.initialize_detection_for_window(&window_id, &session, detection.as_deref());
        // Settled where the injection is finally carried out, so hooks that
        // were dropped on the way (gate, file, shim) neither expect reports
        // nor take part in delivery.
        let provenance = WindowProvenance {
            hooks: provenance.hooks.settle(hooks_injected),
            ..provenance
        };
        if provenance.hooks.reaches() {
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
            Arc::new(OnceLock::from(status_token)),
            input,
            composer_access,
            abandons_calls_at_turn_end,
            definition_name,
            detection_warning.clone(),
            provenance,
            row,
        );

        self.register_window(
            &session,
            entry.clone(),
            detector_handle,
            detected_rx,
            Some(&opening),
        );
        // Registered above, so a hook that spawns a watcher observes an entry
        // the registry can already look up by id.
        if let Some(hook) = lock_recover(&self.window_opened_hook).as_ref() {
            hook(&entry);
        }

        // One trigger point inside `open` covers every open.
        self.request_snapshot();

        Ok(OpenedWindow {
            window_id,
            name: entry.name(),
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
        let opening = lock_recover(&self.opening).len();
        if let Some(index) = self.slot_for_new_entry(&entries, opening)? {
            let evicted = entries.remove(index);
            // The last reference to the entry is not let go under the lock.
            drop(entries);
            drop(evicted);
        }
        Ok(())
    }

    /// Where a new entry fits among `entries` and the `opening` opens in
    /// progress, each of which counts as a living window: `Ok(None)` when it
    /// fits as it is, `Ok(Some(index))` when the entry at `index` must be
    /// evicted first.
    fn slot_for_new_entry(
        &self,
        entries: &[Arc<WindowEntry>],
        opening: usize,
    ) -> Result<Option<usize>, WindowOpenError> {
        let living = entries
            .iter()
            .filter(|entry| entry.current_status() != WindowStatus::Exited)
            .count()
            + opening;
        if living >= self.config.max_windows {
            return Err(WindowOpenError::ResourceExhausted);
        }

        let total_cap = self.config.max_windows.saturating_mul(2);
        if entries.len() + opening < total_cap {
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
            on_name_taken,
            vendor,
            detection,
        } = req.clone();
        let name_supplied = name.is_some();
        let validated = validate_open_request(
            command,
            cwd,
            self.config.default_window_cwd.as_deref(),
            name,
            vendor,
            detection,
        )?;
        let entries = lock_recover(&self.entries);
        let opening = lock_recover(&self.opening);
        self.slot_for_new_entry(&entries, opening.len())?;
        // Only an early refusal, so an approver is not asked about a name
        // that would be refused anyway; the open decides under its lock.
        if name_supplied
            && on_name_taken == OnNameTaken::Refuse
            && let Some(refusal) = name_refusal(&entries, &opening, &validated.name, None)
        {
            return Err(refusal.into());
        }
        Ok(validated.cwd_string)
    }

    /// Renames `entry` to `name` and returns the name as stored, re-read
    /// under the same guard so that concurrent renames converge on one
    /// last-writer-wins value.
    ///
    /// A label change never touches the child process, so this succeeds on an
    /// exited window too. It takes the lock every open takes, so a name
    /// cannot be given to a window that is being opened.
    ///
    /// # Errors
    ///
    /// [`WindowRenameError::Invalid`] if `name` fails
    /// [`validate_window_name`], [`WindowRenameError::NameTaken`] if a
    /// running window other than `entry` carries it or it is a reserved
    /// mention. The stored name is left untouched.
    pub async fn rename_window(
        &self,
        entry: &WindowEntry,
        name: &str,
    ) -> Result<String, WindowRenameError> {
        validate_window_name(name)?;
        let _open_guard = self.open_lock.lock().await;
        let refusal = {
            let entries = lock_recover(&self.entries);
            name_refusal(&entries, &lock_recover(&self.opening), name, Some(entry))
        };
        if let Some(refusal) = refusal {
            return Err(refusal.into());
        }
        Ok(entry.store_name(name))
    }

    /// Makes a [`CliInjection`] ready to spawn with: writes its settings
    /// file and directory when they are needed and not already present, and
    /// returns the argv elements to append. Settings that cannot be written
    /// drop the whole injection (with a warning), since its elements would
    /// name settings the CLI cannot read.
    async fn prepare_injection(&self, injection: CliInjection) -> PreparedInjection {
        let CliInjection {
            kind,
            args,
            config_file,
            config_dir,
        } = injection;
        if config_file.is_none() && config_dir.is_none() {
            return PreparedInjection { kind, args };
        }
        let _write_guard = self.mcp_config_write.lock().await;
        let written = tokio::task::spawn_blocking(move || {
            if let Some(file) = &config_file {
                write_injected_config_file(file).map_err(|err| (file.path(), err))?;
            }
            if let Some(dir) = &config_dir {
                write_injected_config_dir(dir).map_err(|err| (dir.path(), err))?;
            }
            Ok(())
        })
        .await
        .unwrap_or_else(|err| Err((PathBuf::new(), std::io::Error::other(err.to_string()))));
        match written {
            Ok(()) => PreparedInjection { kind, args },
            Err((target, err)) => {
                tracing::warn!(
                    path = %target.display(),
                    error = %err,
                    "could not write the {} settings; the window opens without that injection",
                    kind.label()
                );
                PreparedInjection {
                    kind,
                    args: Vec::new(),
                }
            }
        }
    }

    /// Creates the planned folder `dir` for a `launch` window's credentials
    /// file and settings, or `None` when the window is not one or the folder
    /// cannot be made (the window then opens without hooks and the bridge).
    async fn create_credentials_folder(
        &self,
        mode: CredentialsMode,
        dir: Option<PathBuf>,
    ) -> Option<CredentialsFolder> {
        if mode != CredentialsMode::Launch {
            return None;
        }
        let Some(dir) = dir else {
            tracing::warn!(
                "the window has no usable place to keep its credentials in; it opens without \
                 hooks and the MCP bridge"
            );
            return None;
        };
        match tokio::task::spawn_blocking(move || CredentialsFolder::create_at(dir)).await {
            Ok(Ok(folder)) => Some(folder),
            Ok(Err(err)) => {
                tracing::warn!(
                    error = %err,
                    "could not create the window's credentials folder; the window opens \
                     without hooks and the MCP bridge"
                );
                None
            }
            Err(_) => None,
        }
    }

    /// Writes the credentials of the window `window_id` into `folder`.
    /// Whether it is there: not when the token cannot be verified or the file
    /// cannot be written.
    async fn write_credentials_file(
        folder: Option<&CredentialsFolder>,
        window_id: &str,
        status_token: &StatusToken,
    ) -> bool {
        let Some(folder) = folder else {
            return false;
        };
        let Some(token) = status_token.as_hex().map(str::to_string) else {
            tracing::warn!("the window's token cannot be verified; it gets no credentials file");
            return false;
        };
        let path = folder.file_path();
        let written = tokio::task::spawn_blocking({
            let path = path.clone();
            let window_id = window_id.to_string();
            move || write_credentials(&path, &window_id, &token)
        })
        .await
        .unwrap_or_else(|err| Err(std::io::Error::other(err.to_string())));
        match written {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(error = %err, "could not write the window's credentials file");
                false
            }
        }
    }

    /// Spawns a PTY session with the given command in the resolved working
    /// directory, injecting `YAGURA_STATUS_TOKEN` and `YAGURA_WINDOW_ID` into
    /// the child environment if the token is verifiable (and the window's
    /// credentials are not `launch`ed in a file instead), the MCP bridge path
    /// when one was resolved, plus `extra_profile_env` (a launch profile's
    /// own `[env]` table; empty for an `OpenWindow` request). `injections`
    /// are appended to `argv` in order for this spawn only. The window's
    /// terminal answers color queries with `palette` from the start. The
    /// holder keeps `record` in memory only.
    ///
    /// This is the registry's one spawn call site; `resolution` is what
    /// [`Self::resolve_window_program`] resolved `argv[0]` to.
    // Each argument is a distinct part of one spawn; grouping them would only rename them.
    #[allow(clippy::too_many_arguments)]
    async fn spawn_pty_for_window(
        &self,
        argv: &[String],
        resolved_cwd: &std::path::Path,
        status_token: &StatusToken,
        window_id: &str,
        (record, lost): (String, LostNotice),
        resolution: &program::Resolution,
        extra_profile_env: &[(String, String)],
        injections: &[PreparedInjection],
        palette: Palette,
        credentials: CredentialsMode,
        credentials_folder: Option<&CredentialsFolder>,
    ) -> Result<(PtySession, bool), WindowOpenError> {
        let platform = self.config.host_os.platform();
        let launched = credentials == CredentialsMode::Launch;
        let has_credentials_file = launched
            && Self::write_credentials_file(credentials_folder, window_id, status_token).await;
        let mut child_env = child_environment(
            &self.config,
            status_token,
            window_id,
            extra_profile_env,
            !launched,
        );
        let env_refs = env_refs_names(&self.config, status_token, &child_env, !launched);
        let mut full_argv = argv.to_vec();
        let mut hooks_injected = false;
        for injection in injections {
            // Without its credentials file a `launch` window's hooks and bridge
            // could not claim the window, and its environment holds nothing.
            if launched
                && !has_credentials_file
                && matches!(injection.kind, InjectionKind::Hooks | InjectionKind::Mcp)
            {
                tracing::warn!(
                    "dropped the {} injection: the window has no credentials file",
                    injection.kind.label()
                );
                continue;
            }
            // Filled before the shim check, which judges the argument as spawned.
            let filled = PreparedInjection {
                kind: injection.kind,
                args: injection
                    .args
                    .iter()
                    .map(|arg| crate::profile::fill_env_refs(arg, &env_refs))
                    .collect(),
            };
            let args = injection_args_for(resolution, &filled);
            hooks_injected |= filled.kind == InjectionKind::Hooks && !args.is_empty();
            full_argv.extend_from_slice(args);
        }
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
        let spawn_argv = program::into_spawn_argv(resolution, &full_argv, platform);
        let spawn_argv: Vec<&str> = spawn_argv.iter().map(String::as_str).collect();

        let removed_env: &[&str] = if launched {
            &[WINDOW_ID_ENV, STATUS_TOKEN_ENV]
        } else {
            &[]
        };
        let session = PtySession::spawn_window(
            WindowSpawn {
                window_id,
                argv: &spawn_argv,
                cwd: Some(resolved_cwd),
                extra_env: &extra_env,
                removed_env,
                palette: Some(palette),
                record,
                lost,
            },
            &self.config,
            &self.holder_host,
        )
        .await
        .map_err(|err| WindowOpenError::SpawnFailed(err.to_string()))?;
        Ok((session, hooks_injected))
    }

    /// Resolves `argv[0]` to the executable the window will run, through
    /// `yagura-os` (`resolve_program`): the only place it is resolved. A
    /// resolution the command interpreter would reinterpret is refused
    /// (`InvalidArgument`); one that resolves to nothing
    /// (`Resolution::NotFound`) is kept and left to fail at spawn
    /// (`SpawnFailed`), preserving the `OpenWindow` contract for a program
    /// that simply is not there.
    fn resolve_window_program(
        &self,
        argv: &[String],
    ) -> Result<program::Resolution, WindowOpenError> {
        let predicate = crate::profile::is_executable_predicate(&self.config);
        program::resolve_program(
            argv,
            self.config.host_os.platform(),
            &self.config.program_lookup,
            &predicate,
        )
        .map_err(|rejected| WindowOpenError::InvalidArgument {
            reason: rejected.0,
            argument: None,
        })
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
        self.detection_from(window_id, session, outcome, resolved_name)
    }

    /// The detection a window runs from `outcome`, the load of the definition
    /// named `resolved_name`.
    fn detection_from(
        &self,
        window_id: &str,
        session: &PtySession,
        outcome: DetectionLoadOutcome,
        resolved_name: String,
    ) -> DetectionSetup {
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

    /// This registry's boot nonce: what identifies this daemon run, and ends
    /// every approval request id minted in it. Window ids do not carry it.
    #[must_use]
    pub fn boot_nonce(&self) -> &str {
        self.holder_host.boot()
    }

    /// Picks a window id that no window, holder's files or row of the window
    /// table has, and reserves it by writing the window's `opening` row with
    /// `values`. Called under `open_lock`, so two opens never pick one id.
    async fn reserve_window_id(
        &self,
        values: &WindowValues,
    ) -> Result<(String, OpeningRow), WindowOpenError> {
        let table = lock_recover(&self.window_table).clone();
        let holders_dir = self.holder_host.holders_dir();
        for _ in 0..WINDOW_ID_ROUNDS {
            let candidates = (0..WINDOW_ID_CANDIDATES)
                .map(|_| random_window_id())
                .collect::<Result<Vec<_>, _>>()?;
            let in_use: HashSet<String> = {
                let entries = lock_recover(&self.entries);
                let opening = lock_recover(&self.opening);
                entries
                    .iter()
                    .map(|entry| entry.window_id.clone())
                    .chain(opening.keys().cloned())
                    .collect()
            };
            let in_use = |id: &str| in_use.contains(id);
            let picked = match &holders_dir {
                Some(dir) => first_free_window_id(candidates, dir, in_use),
                // No holder has been started yet, so none has files.
                None => candidates.into_iter().find(|id| !in_use(id)),
            };
            let Some(window_id) = picked else {
                continue;
            };
            if let Reservation::Reserved(row) =
                OpeningRow::reserve(table.as_ref(), &window_id, values).await
            {
                return Ok((window_id, row));
            }
        }
        Err(WindowOpenError::SpawnFailed(
            "no free window id was found".to_owned(),
        ))
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
        status_token: TokenSlot,
        input: InputProfile,
        composer_access: ComposerAccess,
        abandons_calls_at_turn_end: bool,
        detection_definition: Option<String>,
        detection_warning: Option<String>,
        provenance: WindowProvenance,
        row: WindowRow,
    ) -> Arc<WindowEntry> {
        // The same rule the engine itself applies at
        // construction -- a resolved definition means nothing has been
        // judged yet, no definition at all pins the window to that reason
        // instead.
        let initial_status = if detection_definition.is_some() {
            DetectedStatus::Unknown(UnknownReason::NotYetJudged)
        } else {
            DetectedStatus::Unknown(UnknownReason::NoDefinition)
        };
        let status_gate = StatusGate::new(
            initial_status,
            tokio::time::Instant::now(),
            ReadyHoldLatch::new(input.ready_hold),
        );
        let delivery_spec = provenance
            .delivery
            .as_ref()
            .map_or_else(|| self.config().delivery.clone(), ProfileDelivery::spec);
        let inbox = Inbox::new(
            self.config().inbox_capacity,
            delivery_spec,
            status_gate.watch(),
        );
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
            status_gate,
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
            agent_running: Mutex::new(false),
            notice_wake: Notify::new(),
            notice_attempts: AtomicU64::new(0),
            inbox,
            session_id: Mutex::new(None),
            provenance,
            mcp_connections: Arc::new(AtomicUsize::new(0)),
            participation: Arc::clone(&self.participation),
            row,
        })
    }

    /// Registers the window entry in the registry, handing it the seat and
    /// name of the open in progress (`opening`) if any, and spawns all
    /// background tasks (output pump, exit watcher, status forwarder).
    fn register_window(
        &self,
        session: &Arc<PtySession>,
        entry: Arc<WindowEntry>,
        detector_handle: Arc<DetectorHandle>,
        detected_rx: mpsc::UnboundedReceiver<ConfirmedStatus>,
        opening: Option<&OpeningWindow<'_>>,
    ) {
        // The output pump is already running (started right after the
        // detector was created, see `open_impl`); exactly `entry` and this
        // watcher task hold a strong `Arc<DetectorHandle>` from here on.
        spawn_exit_watcher(
            session.clone(),
            detector_handle.clone(),
            Arc::downgrade(&entry),
        );
        spawn_status_forwarder(Arc::downgrade(&entry), detected_rx);
        drop(detector_handle);

        {
            // Under the entries lock, so a count never sees the window both
            // as an entry and as an open in progress, nor as neither.
            let mut entries = lock_recover(&self.entries);
            entries.push(entry);
            if let Some(opening) = opening {
                opening.hand_over();
            }
        }
        self.participation.raise();
    }

    /// A receiver that wakes whenever the set of windows, or a name or
    /// status a chat roster shows, may have changed.
    pub(crate) fn subscribe_participation(&self) -> watch::Receiver<u64> {
        self.participation.0.subscribe()
    }

    /// Raises the participation signal for a change the registry cannot see
    /// itself, such as a chat room becoming available for a window.
    pub(crate) fn announce_participation_change(&self) {
        self.participation.raise();
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

    /// Marks `window_id` as being opened under `name` until the returned
    /// value is dropped or the window is registered.
    pub(crate) fn begin_opening(&self, window_id: &str, name: &str) -> OpeningWindow<'_> {
        open_in_progress(&self.opening, window_id, name)
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

    /// Installs the window table every window opened from now on writes its
    /// row in (`store::wire_into` is its only caller).
    pub(crate) fn set_window_table(&self, table: WindowTable) {
        *lock_recover(&self.window_table) = Some(table);
    }

    /// The table every window's row goes in; `None` when the registry has no
    /// database.
    pub(crate) fn window_table(&self) -> Option<WindowTable> {
        lock_recover(&self.window_table).clone()
    }

    /// The directory holders that outlive this daemon keep their records in;
    /// `None` when this registry's holders end with it.
    pub(crate) fn lasting_holders_dir(&self) -> Option<PathBuf> {
        self.holder_host
            .take_back_context()
            .map(|context| context.holders_dir)
    }

    /// Registers the window `survivor` names, which a previous daemon of the
    /// instance left running, under its id: not connected and `Unknown`,
    /// listed, closable and stopped with the daemon from now on. Its link
    /// lets itself into the holder once `begin` turns `true`; the holder's
    /// launch record then gives back the window's token. A window with a row
    /// takes the values the row keeps; one without takes only its id, and the
    /// values the launch record keeps (with its row written again) once the
    /// holder lets the daemon in. Not refused by the window limits: the window
    /// already exists. The chat introduction is not sent again.
    ///
    /// # Errors
    ///
    /// [`WindowOpenError::SpawnFailed`] when the window's link cannot start.
    pub(crate) async fn take_back(
        self: &Arc<Self>,
        survivor: crate::recovery::Survivor,
        begin: watch::Receiver<bool>,
    ) -> Result<(), WindowOpenError> {
        let crate::recovery::Survivor {
            window_id,
            stored,
            record,
            holder,
        } = survivor;
        let status_token: TokenSlot = Arc::default();
        let (recorded_tx, recorded_rx) = oneshot::channel::<WindowValues>();
        let needs_values = stored.is_none();
        let on_welcome: Box<dyn FnOnce(&str) + Send> = {
            let status_token = Arc::clone(&status_token);
            let window_id = window_id.clone();
            Box::new(move |record: &str| {
                let recorded = recorded_launch(record, &window_id);
                let token = recorded
                    .as_ref()
                    .and_then(|recorded| recorded.token.as_deref())
                    .and_then(StatusToken::from_override);
                match token {
                    Some(token) => {
                        let _ = status_token.set(Arc::new(token));
                    }
                    None => tracing::warn!(
                        window_id,
                        "the holder's launch record carries no token for the window; its status events are refused"
                    ),
                }
                if needs_values && let Some(recorded) = recorded {
                    let _ = recorded_tx.send(recorded.values);
                }
            })
        };
        let lost: LostNotice = {
            let table = self.window_table();
            let window_id = window_id.clone();
            Box::new(move || {
                if let Some(table) = table {
                    table.mark_lost(&window_id);
                }
            })
        };
        let (values, row, known) = match stored {
            Some(stored) => (
                stored.values,
                WindowRow::taken_back(self.window_table(), &window_id),
                true,
            ),
            None => (
                unknown_values(&window_id),
                WindowRow::taken_back(None, &window_id),
                false,
            ),
        };
        let session = PtySession::take_back(
            TakeBackSpawn {
                window_id: &window_id,
                command: &values.command,
                record,
                holder,
                begin,
                on_welcome,
                lost,
            },
            &self.config,
            &self.holder_host,
        )
        .map_err(|err| WindowOpenError::SpawnFailed(err.to_string()))?;
        let session = Arc::new(session);
        let (entry, detector_handle, detected_rx) = self
            .taken_back_entry(&window_id, values, known, &session, status_token, row)
            .await?;
        self.register_window(&session, entry.clone(), detector_handle, detected_rx, None);
        if needs_values {
            let registry = Arc::downgrade(self);
            tokio::spawn(async move {
                let Ok(values) = recorded_rx.await else {
                    return;
                };
                if let Some(registry) = registry.upgrade() {
                    registry.fill_taken_back(&entry, values).await;
                }
            });
        }
        self.request_snapshot();
        Ok(())
    }

    /// Replaces `placeholder`, a window taken back without a row, with the
    /// same window built from `values`, which its holder's launch record kept,
    /// and writes its row again. A window closed or removed meanwhile is left
    /// so.
    async fn fill_taken_back(&self, placeholder: &Arc<WindowEntry>, values: WindowValues) {
        let window_id = placeholder.window_id.clone();
        let row = match self.window_table() {
            Some(table) => table.restore_row(&window_id, &values).await,
            None => WindowRow::taken_back(None, &window_id),
        };
        let built = self
            .taken_back_entry(
                &window_id,
                values,
                true,
                &placeholder.session,
                Arc::clone(&placeholder.status_token),
                row,
            )
            .await;
        let (entry, detector_handle, detected_rx) = match built {
            Ok(built) => built,
            Err(error) => {
                tracing::warn!(window_id, %error, "cannot give the window taken back its values");
                return;
            }
        };
        {
            let mut entries = lock_recover(&self.entries);
            let Some(slot) = entries
                .iter_mut()
                .find(|entry| Arc::ptr_eq(entry, placeholder))
            else {
                return;
            };
            if placeholder.closed.load(Ordering::SeqCst) {
                return;
            }
            *slot = Arc::clone(&entry);
        }
        spawn_exit_watcher(
            Arc::clone(&placeholder.session),
            detector_handle,
            Arc::downgrade(&entry),
        );
        spawn_status_forwarder(Arc::downgrade(&entry), detected_rx);
        self.participation.raise();
        self.request_snapshot();
    }

    /// A window taken back, built from `values` over `session`: with the
    /// detection they name and what its profile determines when they are
    /// `known`, without a definition (so its status stays `Unknown`) when they
    /// are only a placeholder.
    async fn taken_back_entry(
        &self,
        window_id: &str,
        values: WindowValues,
        known: bool,
        session: &Arc<PtySession>,
        status_token: TokenSlot,
        row: WindowRow,
    ) -> Result<
        (
            Arc<WindowEntry>,
            Arc<DetectorHandle>,
            mpsc::UnboundedReceiver<ConfirmedStatus>,
        ),
        WindowOpenError,
    > {
        let workspace = resolve_key_blocking(PathBuf::from(&values.cwd)).await;
        let detection = if known {
            self.initialize_detection_for_window(window_id, session, values.detection.as_deref())
        } else {
            self.detection_from(
                window_id,
                session,
                DetectionLoadOutcome::Failed(
                    "the window's values are not known until its holder lets the daemon in"
                        .to_owned(),
                ),
                String::new(),
            )
        };
        let DetectionSetup {
            handle: detector_handle,
            detected_rx,
            warning: detection_warning,
            input,
            definition_name,
            composer_access,
            abandons_calls_at_turn_end,
        } = detection;
        let profile = values
            .profile
            .as_deref()
            .filter(|_| known)
            .map(|profile| profile_determined(&self.config, window_id, profile))
            .unwrap_or_default();
        let provenance = WindowProvenance {
            opener: values.opener.clone(),
            task_origin: values
                .task_plan_id
                .clone()
                .zip(values.task_id.clone())
                .map(|(plan_id, task_id)| TaskOrigin { plan_id, task_id }),
            launch: values.profile.clone().map(|profile| LaunchIdentity {
                profile,
                model: values.model.clone(),
                effort: values.effort.clone(),
            }),
            delivery: profile.delivery,
            hooks: profile.hooks,
            wake_command: profile.wake_command,
            credentials: credentials_mode(values.credentials),
            // Only an open creates the folder; a window taken back has its own.
            credentials_dir: None,
        };
        if provenance.hooks.reaches() {
            detector_handle.expect_status_reports(true);
        }
        spawn_output_pump(session, Arc::downgrade(&detector_handle));
        let name = {
            let entries = lock_recover(&self.entries);
            let opening = lock_recover(&self.opening);
            claim_name(&entries, &opening, &values.name, OnNameTaken::Number)?
        };
        let entry = self.create_window_entry(
            window_id.to_owned(),
            name,
            values.vendor.clone(),
            values.cwd.clone(),
            workspace,
            values.command.clone(),
            values.detection.clone(),
            Arc::clone(session),
            detector_handle.clone(),
            status_token,
            input,
            composer_access,
            abandons_calls_at_turn_end,
            definition_name,
            detection_warning,
            provenance,
            row,
        );
        if let Some(layout_json) = values.layout_json {
            entry.set_layout(layout_json);
        }
        if let Some(session_id) = values.session_id.as_deref() {
            entry.record_session_id(session_id);
        }
        Ok((entry, detector_handle, detected_rx))
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
    /// window whose inbox already holds something: a window opened before the
    /// IPC server existed (the daemon's first) must not wait for another
    /// delivery to be woken.
    pub(crate) fn set_notice_launcher(&self, launcher: NoticeLauncher) {
        // Held across the scan, so a notice queued meanwhile finds this
        // launcher installed rather than none.
        let mut installed = lock_recover(&self.notice_launcher);
        let waiting: Vec<Arc<WindowEntry>> = lock_recover(&self.entries)
            .iter()
            .filter(|entry| entry.inbox.pending_count() > 0)
            .cloned()
            .collect();
        for entry in &waiting {
            launcher(entry);
        }
        *installed = Some(launcher);
    }

    /// Whether every window is being terminated with the daemon.
    pub(crate) fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    /// Starts `entry`'s notice delivery agent, or wakes the running one. A
    /// no-op until a launcher is installed.
    pub(crate) fn wake_notices(&self, entry: &Arc<WindowEntry>) {
        if let Some(launcher) = lock_recover(&self.notice_launcher).as_ref() {
            launcher(entry);
        }
    }

    /// Whether a window that has not exited belongs to the workspace room
    /// `room_id`: only a running window can use chat and so re-create the
    /// room, and the GUI shows no pane for an exited one. The one answer to
    /// "is this room in use", for the listing and for refusing a delete.
    pub fn has_window_in_room(&self, room_id: &str) -> bool {
        lock_recover(&self.entries).iter().any(|entry| {
            entry.current_status() != WindowStatus::Exited
                && workspace::room_id_for(&entry.workspace) == room_id
        })
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
                delivers: entry.delivers(),
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

    /// Terminates every window's child process, then waits, at most
    /// `close_grace_ms` for all of them together, for every window's holder
    /// process to end. Statuses converge to `Exited`
    /// asynchronously as each session's exit is observed; this method does
    /// not wait for that convergence.
    pub async fn terminate_all(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        let entries: Vec<Arc<WindowEntry>> = lock_recover(&self.entries).clone();
        for entry in &entries {
            entry
                .remove_row_then(async { entry.session.terminate() })
                .await;
        }
        let limit = Duration::from_millis(self.config.close_grace_ms);
        let deadline = tokio::time::Instant::now() + limit;
        for entry in &entries {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if !entry.session.wait_until_released(left).await {
                tracing::warn!(
                    window_id = entry.window_id(),
                    "a window's holder had not ended within the close grace"
                );
            }
        }
    }
}

/// The values a window taken back without a row has until its holder's launch
/// record gives its own: its id for a name, and nothing else.
fn unknown_values(window_id: &str) -> WindowValues {
    WindowValues {
        name: window_id.to_owned(),
        cwd: String::new(),
        vendor: None,
        command: Vec::new(),
        detection: None,
        profile: None,
        model: None,
        effort: None,
        layout_json: None,
        session_id: None,
        opener: None,
        task_plan_id: None,
        task_id: None,
        credentials: credentials_kind(CredentialsMode::Env),
    }
}

/// What a window's profile determines and its row does not keep.
#[derive(Default)]
struct ProfileDetermined {
    delivery: Option<ProfileDelivery>,
    hooks: HooksRoute,
    wake_command: Option<WakeCommand>,
}

/// What the profile `profile` determines for a window taken back, resolved
/// against the profiles this daemon loaded the way an open plans it: its
/// `[delivery]`, the route its hooks take, and its wake command. The defaults,
/// with one line logged, when the profile cannot be launched any more. How an
/// open settled its hooks (left out or not) is not kept, so the route is the
/// one the profile plans.
fn profile_determined(config: &DaemonConfig, window_id: &str, profile: &str) -> ProfileDetermined {
    let launch = match crate::profile::resolve_launch(
        config,
        profile,
        None,
        None,
        &crate::profile::LaunchOptionValues::default(),
    ) {
        Ok(launch) => launch,
        Err(error) => {
            tracing::warn!(
                window_id,
                profile,
                %error,
                "the profile of a window taken back cannot be resolved; its delivery, hooks and wake command take the defaults"
            );
            return ProfileDetermined::default();
        }
    };
    let hooks = if launch.hooks.is_some() {
        HooksRoute::Injection
    } else if launch
        .hooks_registration
        .as_ref()
        .is_some_and(crate::mcp_registration::McpRegistrationSpec::owns_entries)
    {
        HooksRoute::RegisteredEntries
    } else {
        HooksRoute::Unarranged
    };
    let wake_command = launch
        .delivery
        .as_ref()
        .filter(|delivery| matches!(delivery.wake, crate::profile_delivery::WakeMode::Command(_)))
        .map(|_| WakeCommand {
            argv: launch.argv.clone(),
            env: launch.env.clone().into_iter().collect(),
        });
    ProfileDetermined {
        delivery: launch.delivery,
        hooks,
        wake_command,
    }
}

/// Each in-progress open's window id, mapped to what it holds.
type OpeningWindows = Mutex<HashMap<String, Opening>>;

/// An open in progress: a seat under the window limits and its name.
struct Opening {
    name: String,
    /// Closes when the open finishes.
    released: watch::Receiver<()>,
}

/// One open in progress, from [`WindowRegistry::begin_opening`] until drop.
/// Dropping it removes the id first and then closes the channel, so a waiter
/// woken by the close always finds the open settled.
pub(crate) struct OpeningWindow<'a> {
    opening: &'a OpeningWindows,
    window_id: String,
    _released: watch::Sender<()>,
}

fn open_in_progress<'a>(
    opening: &'a OpeningWindows,
    window_id: &str,
    name: &str,
) -> OpeningWindow<'a> {
    let (released, pending) = watch::channel(());
    lock_recover(opening).insert(
        window_id.to_string(),
        Opening {
            name: name.to_string(),
            released: pending,
        },
    );
    OpeningWindow {
        opening,
        window_id: window_id.to_string(),
        _released: released,
    }
}

impl OpeningWindow<'_> {
    /// Gives up the seat and the name to the registered entry; a waiter is
    /// still woken only on drop.
    fn hand_over(&self) {
        lock_recover(self.opening).remove(&self.window_id);
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
    let pending = lock_recover(opening)
        .get(window_id)
        .map(|open| open.released.clone());
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
/// without auth cannot authenticate status events -- and only when
/// `carries_credentials`: a `launch` window gets them in a file instead), the MCP bridge path when
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
    carries_credentials: bool,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = Vec::new();
    let mut forwarded: Vec<String> = channel_env_names(status_token, carries_credentials)
        .into_iter()
        .map(str::to_string)
        .collect();
    if let Some(hex) = status_token.as_hex().filter(|_| carries_credentials) {
        env.push((STATUS_TOKEN_ENV.to_string(), hex.to_string()));
        env.push((WINDOW_ID_ENV.to_string(), window_id.to_string()));
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

/// The MCP channel variables a window's child receives: its identity pair
/// when its token is verifiable and the window carries its credentials in the
/// environment, and the instance name when the daemon has one (inherited,
/// never set). The one source for the `WSLENV` forwarding and for
/// `{env_refs}`.
fn channel_env_names(status_token: &StatusToken, carries_credentials: bool) -> Vec<&'static str> {
    yagura_protocol::MCP_CHANNEL_ENV_VARS
        .into_iter()
        .filter(|name| match *name {
            INSTANCE_ENV => std::env::var_os(INSTANCE_ENV).is_some(),
            _ => carries_credentials && status_token.as_hex().is_some(),
        })
        .collect()
}

/// The variables `{env_refs}` names for one window: [`channel_env_names`]
/// plus `WSLENV` when the child ends up with one (set by
/// [`child_environment`] or inherited).
fn env_refs_names(
    config: &DaemonConfig,
    status_token: &StatusToken,
    child_env: &[(String, String)],
    carries_credentials: bool,
) -> Vec<&'static str> {
    let mut names = channel_env_names(status_token, carries_credentials);
    let has_wslenv = child_env
        .iter()
        .any(|(key, _)| key.eq_ignore_ascii_case(WSLENV))
        || config
            .child_base_env
            .get(std::ffi::OsStr::new(WSLENV))
            .is_some()
        || std::env::var_os(WSLENV).is_some();
    if has_wslenv {
        names.push(WSLENV);
    }
    names
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

/// The name a window's credentials file has inside its folder.
const CREDENTIALS_FILE_NAME: &str = "credentials.json";

/// What a window's credentials folder is named after, followed by a random
/// part; the start-up sweep removes the folders that carry it.
const CREDENTIALS_FOLDER_PREFIX: &str = "credentials-";

/// A `launch` window's own folder under the MCP config directory, holding the
/// file only its hooks and bridge are told of. Dropping it removes the folder
/// with everything in it, so every way a window's open or life ends clears it.
struct CredentialsFolder {
    dir: PathBuf,
}

impl CredentialsFolder {
    /// Blocking: makes the folder `dir` (from [`plan_credentials_folder`]),
    /// owner-only.
    fn create_at(dir: PathBuf) -> std::io::Result<Self> {
        // Owns the path before anything exists, so a partial creation is cleared.
        let folder = Self { dir };
        let base = folder.dir.parent().and_then(Path::parent);
        yagura_os::secure_owned_dir(base.unwrap_or(&folder.dir), &folder.dir)
            .map_err(std::io::Error::other)?;
        std::fs::create_dir_all(&folder.dir)?;
        Ok(folder)
    }

    fn file_path(&self) -> PathBuf {
        self.dir.join(CREDENTIALS_FILE_NAME)
    }
}

/// Where the next `launch` window's folder goes under `config_dir`: a random
/// name that does not depend on the window id (not yet allocated). Nothing is
/// created.
///
/// # Errors
///
/// The system's random source failing.
pub(crate) fn plan_credentials_folder(config_dir: &Path) -> std::io::Result<PathBuf> {
    let mut random = [0u8; 16];
    yagura_os::fill_random(&mut random).map_err(|err| std::io::Error::other(err.to_string()))?;
    let name: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(config_dir.join(format!("{CREDENTIALS_FOLDER_PREFIX}{name}")))
}

/// The path of the credentials file in the folder `dir` as a hook, bridge or
/// settings file names it: absolute, and on Windows with `/` separators, which
/// both Windows and Git Bash read. `None` when it holds a character that would
/// break out of a JSON string, a TOML string or a single-quoted shell word.
pub(crate) fn credentials_file_argument(dir: &Path) -> Option<String> {
    let path = dir.join(CREDENTIALS_FILE_NAME).to_str()?.to_string();
    let path = if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path
    };
    (!path.contains(['\'', '"', '\\']) && !path.chars().any(char::is_control)).then_some(path)
}

impl Drop for CredentialsFolder {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_dir_all(&self.dir)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(error = %err, "could not remove a window's credentials folder");
        }
    }
}

/// Blocking: writes `{"window_id", "token"}` to `path`, readable by its owner
/// only (the folder it is in is owner-only too).
fn write_credentials(path: &Path, window_id: &str, token: &str) -> std::io::Result<()> {
    use std::io::Write as _;

    let body = serde_json::json!({ "window_id": window_id, "token": token }).to_string();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(body.as_bytes())?;
    let _ = yagura_os::secure_owned_file(path);
    Ok(())
}

/// Removes `folder` once the child of `session` has exited, however it ended.
fn remove_credentials_when_exited(session: &PtySession, folder: CredentialsFolder) {
    let mut exited = session.exit_notified();
    tokio::spawn(async move {
        // Also returns when the session is gone without having reported an exit.
        let _ = exited.wait_for(|exited| *exited).await;
        drop(folder);
    });
}

/// Blocking: removes what earlier daemon runs of this instance left in
/// `config_dir`. Run once the instance's endpoint is held, so no live window
/// of another daemon can own what is removed.
pub fn remove_stale_credentials(config_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(config_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let is_credentials_folder = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(CREDENTIALS_FOLDER_PREFIX));
        let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
        if is_credentials_folder
            && is_dir
            && let Err(err) = std::fs::remove_dir_all(entry.path())
        {
            tracing::warn!(error = %err, "could not remove a stale credentials folder");
        }
    }
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

/// Blocking: publishes `dir` unless a directory of that name is already
/// there. The name is derived from the files, so an existing one holds them.
fn write_injected_config_dir(dir: &InjectedConfigDir) -> std::io::Result<()> {
    let base = dir.parent.parent().unwrap_or(&dir.parent);
    let files: Vec<(&str, &str)> = dir
        .files
        .iter()
        .map(|(name, content)| (name.as_str(), content.as_str()))
        .collect();
    yagura_os::publish_config_dir(base, &dir.parent, &dir.dir_name, &files)
}

/// Why `name` cannot go to a window other than `except`, if it cannot: it is
/// a reserved mention, or a running window (any state but `Exited`) or an
/// open in progress already carries a name the mention rule treats as the
/// same.
fn name_refusal(
    entries: &[Arc<WindowEntry>],
    opening: &HashMap<String, Opening>,
    name: &str,
    except: Option<&WindowEntry>,
) -> Option<NameRefusal> {
    if reserved_mention(name).is_some() {
        return Some(NameRefusal::Reserved(name.to_string()));
    }
    let key = mention_key(name);
    let running = entries
        .iter()
        .filter(|entry| except.is_none_or(|except| entry.window_id() != except.window_id()))
        .filter(|entry| entry.current_status() != WindowStatus::Exited)
        .any(|entry| mention_key(&lock_recover(&entry.name)) == key);
    let opening = opening.values().any(|open| mention_key(&open.name) == key);
    (running || opening).then(|| NameRefusal::InUse(name.to_string()))
}

/// The name a new window takes: `requested` itself when free, otherwise a
/// refusal or, when asked to number, `<requested> <n>` with the smallest
/// free `n` from 2.
fn claim_name(
    entries: &[Arc<WindowEntry>],
    opening: &HashMap<String, Opening>,
    requested: &str,
    on_name_taken: OnNameTaken,
) -> Result<String, NameRefusal> {
    let Some(refusal) = name_refusal(entries, opening, requested, None) else {
        return Ok(requested.to_string());
    };
    if on_name_taken == OnNameTaken::Refuse {
        return Err(refusal);
    }
    (2u64..)
        .map(|n| numbered_name(requested, n))
        .find(|candidate| name_refusal(entries, opening, candidate, None).is_none())
        .ok_or(refusal)
}

/// `<base> <n>`, with `base` cut at a character boundary so the whole name
/// stays within [`WINDOW_NAME_MAX_BYTES`].
fn numbered_name(base: &str, n: u64) -> String {
    let suffix = format!(" {n}");
    let mut end = base
        .len()
        .min(WINDOW_NAME_MAX_BYTES.saturating_sub(suffix.len()));
    while !base.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}", &base[..end])
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

    let resolved_cwd = resolve_window_cwd(cwd.as_ref(), default_cwd)?;
    let cwd_string = resolved_cwd.to_string_lossy().to_string();

    Ok(ValidatedRequest {
        name,
        vendor,
        resolved_cwd,
        cwd_string,
    })
}

/// The directory a window opened with `cwd` starts in: `cwd`, else
/// `default_cwd` ([`DaemonConfig::default_window_cwd`]), else the daemon's
/// own current directory, each held to [`resolve_existing_dir`].
///
/// A start-up default that has since disappeared is refused rather than
/// swapped for the daemon's directory, so a window never opens somewhere
/// nobody chose without a word.
///
/// # Errors
///
/// [`WindowOpenError::InvalidArgument`] when the chosen directory does not
/// resolve to an existing one, or the daemon's current directory cannot be
/// read.
pub(crate) fn resolve_window_cwd(
    cwd: Option<&RequestedCwd>,
    default_cwd: Option<&Path>,
) -> Result<PathBuf, WindowOpenError> {
    match (cwd, default_cwd) {
        (Some(requested), _) => resolve_existing_dir(requested.path(), requested.origin()),
        (None, Some(default)) => resolve_existing_dir(default, CwdOrigin::DaemonDefault),
        (None, None) => {
            let current = std::env::current_dir().map_err(|err| {
                WindowOpenError::invalid_cwd(
                    CwdOrigin::DaemonCurrentDir,
                    format!("failed to resolve the daemon's current directory: {err}"),
                )
            })?;
            resolve_existing_dir(&current, CwdOrigin::DaemonCurrentDir)
        }
    }
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
        Some(_) => ComposerAccess::Judged,
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

/// Tells the detector each time a copy of the window's screen holding more
/// output was applied (or a copy was refused),
/// independently of whether a client is attached: detection must keep
/// converging while the GUI is away. The detector reads the copy itself, so
/// arrivals that pile up collapse into one and nothing is lost. Holds only a
/// [`Weak`] reference to the detector so this task can never be the thing
/// keeping `Exited` teardown from completing; it upgrades on every arrival
/// and breaks as soon as that fails.
pub(crate) fn spawn_output_pump(session: &PtySession, detector: Weak<DetectorHandle>) {
    let mut arrivals = session.output_arrivals();
    // Output the child wrote before this call is already in the copy.
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

/// Turns the child-exit fact into the window's confirmed `Exited`
/// ([`WindowEntry::confirm_exited`]) and into the detector's
/// highest-priority evidence. One of the two intended long-lived strong
/// holders of `Arc<DetectorHandle>`: this task's own clone is dropped when it
/// returns, so the entry taking its detector never invalidates it.
///
/// Holds only a [`Weak<WindowEntry>`], so it never keeps an evicted entry
/// alive. The process tree and the PTY are wound down by the session's own
/// exit monitor ([`PtySession`]), so this task has no teardown of its own.
pub(crate) fn spawn_exit_watcher(
    session: Arc<PtySession>,
    detector: Arc<DetectorHandle>,
    entry: Weak<WindowEntry>,
) {
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
            "child process exited; confirming the window exited"
        );
        detector.notify_exited();
        if let Some(entry) = entry.upgrade() {
            let _ = entry.confirm_exited();
        }
    });
}

/// Consumes one window's confirmed-status stream and publishes each status
/// through [`WindowEntry::publish_confirmed`]. Once the stream ends -- the
/// driver task finished or died -- it fails a window that has not exited
/// back to `Unknown(DetectorStopped)`.
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
            entry.publish_confirmed(confirmed);
        }
        if let Some(entry) = entry.upgrade() {
            entry.publish_detector_loss();
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// On a paused clock, reached only once nothing else can run: a lookup
    /// that has not finished by then is waiting on something other than time.
    const UNTIL_IDLE: Duration = Duration::from_secs(3600);

    #[tokio::test(start_paused = true)]
    async fn a_lookup_waits_for_an_open_in_progress_and_then_finds_the_registered_window() {
        let opening = OpeningWindows::default();
        let registered: Mutex<Option<&str>> = Mutex::new(None);
        let guard = open_in_progress(&opening, "w-pending", "pending");

        let lookup = lookup_once_opened(&opening, "w-pending", || *lock_recover(&registered));
        tokio::pin!(lookup);
        assert!(
            tokio::time::timeout(UNTIL_IDLE, &mut lookup).await.is_err(),
            "a lookup must wait while the open of that id is still in progress"
        );

        *lock_recover(&registered) = Some("entry");
        drop(guard);
        let found = tokio::time::timeout(UNTIL_IDLE, lookup)
            .await
            .expect("the lookup must end once the open ends");
        assert_eq!(found, Some("entry"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_lookup_for_an_open_that_fails_finds_nothing_once_it_ends() {
        let opening = OpeningWindows::default();
        let guard = open_in_progress(&opening, "w-pending", "pending");

        let lookup = lookup_once_opened(&opening, "w-pending", || None::<&str>);
        tokio::pin!(lookup);
        assert!(
            tokio::time::timeout(UNTIL_IDLE, &mut lookup).await.is_err(),
            "a lookup must wait while the open of that id is still in progress"
        );

        drop(guard);
        let found = tokio::time::timeout(UNTIL_IDLE, lookup)
            .await
            .expect("the lookup must end once the open ends");
        assert_eq!(found, None);
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_an_open_in_progress_always_removes_it() {
        let registry = WindowRegistry::new(DaemonConfig::default());
        let guard = registry.begin_opening("w-pending", "pending");
        assert!(lock_recover(&registry.opening).contains_key("w-pending"));

        drop(guard);
        assert!(lock_recover(&registry.opening).is_empty());
        let settled = tokio::time::timeout(UNTIL_IDLE, registry.get_after_open("w-pending"))
            .await
            .expect("no open is in progress, so the lookup must not wait");
        assert!(settled.is_none());
    }

    fn named_echo_child_request(name: &str) -> OpenWindowRequest {
        OpenWindowRequest {
            command: yagura_test_support::process::echo_child_command(),
            cwd: None,
            name: Some(name.to_string()),
            vendor: None,
            detection: None,
            on_name_taken: OnNameTaken::Refuse,
        }
    }

    #[tokio::test]
    async fn an_open_in_progress_holds_its_name_against_another_open_and_a_rename() {
        let registry = WindowRegistry::new(DaemonConfig::default());
        let other = registry
            .open(named_echo_child_request("other"))
            .await
            .expect("an echo_child window must open");
        let held = registry.begin_opening("w-held", "held");

        let reopened = registry
            .open(named_echo_child_request("held"))
            .await
            .map(|opened| opened.name);
        let renamed = registry.rename_window(&other.entry, "held").await;
        drop(held);
        let renamed_once_ended = registry.rename_window(&other.entry, "held").await;
        // Before the asserts, so a refusal that failed leaves no child behind.
        registry.terminate_all().await;

        assert!(
            matches!(
                reopened,
                Err(WindowOpenError::NameTaken(NameRefusal::InUse(_)))
            ),
            "an open must not take the name of an open still in progress: {reopened:?}"
        );
        assert!(
            matches!(
                renamed,
                Err(WindowRenameError::NameTaken(NameRefusal::InUse(_)))
            ),
            "a rename must not take the name of an open still in progress: {renamed:?}"
        );
        assert_eq!(
            renamed_once_ended.ok().as_deref(),
            Some("held"),
            "once the open in progress ends, its name is free again"
        );
    }

    #[tokio::test]
    async fn a_window_is_counted_once_from_the_moment_it_is_handed_over() {
        let registry = Arc::new(WindowRegistry::new(DaemonConfig::default()));
        let counted: Arc<Mutex<Option<(usize, usize)>>> = Arc::default();
        let hook_registry = Arc::downgrade(&registry);
        let hook_counted = Arc::clone(&counted);
        // Runs after the hand-over and before the open's guard drops.
        registry.set_window_opened_hook(Box::new(move |_entry| {
            let Some(registry) = hook_registry.upgrade() else {
                return;
            };
            let entries = lock_recover(&registry.entries).len();
            let opening = lock_recover(&registry.opening).len();
            *lock_recover(&hook_counted) = Some((entries, opening));
        }));

        registry
            .open(named_echo_child_request("counted"))
            .await
            .expect("an echo_child window must open");
        registry.terminate_all().await;

        assert_eq!(
            *lock_recover(&counted),
            Some((1, 0)),
            "a registered window must be one entry and no longer an open in progress"
        );
    }

    #[tokio::test]
    async fn an_open_in_progress_holds_a_seat_against_the_window_limit() {
        let registry = WindowRegistry::new(DaemonConfig {
            max_windows: 1,
            ..DaemonConfig::default()
        });
        let request = named_echo_child_request("next");
        let before = registry.check_open_request(&request).map(|_| ());

        let held = registry.begin_opening("w-held", "held");
        let while_held = registry.check_open_request(&request).map(|_| ());
        drop(held);

        assert!(
            before.is_ok(),
            "with no window and no open in progress, one window fits under a limit of one: \
             {before:?}"
        );
        assert!(
            matches!(while_held, Err(WindowOpenError::ResourceExhausted)),
            "an open in progress takes the only seat a limit of one has: {while_held:?}"
        );
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
                on_name_taken: Default::default(),
            })
            .await
            .expect("an echo_child window must open");
        let entry = opened.entry;

        assert!(entry.claim_notice_agent());
        assert!(
            !entry.claim_notice_agent(),
            "a second claim while an agent runs must be refused, or two agents deliver one notice"
        );
        entry.release_notice_agent();
        assert!(
            entry.claim_notice_agent(),
            "a released agent must leave the window claimable again"
        );
        registry.terminate_all().await;
    }

    #[tokio::test]
    async fn the_boot_nonce_differs_per_registry() {
        let registry = WindowRegistry::new(DaemonConfig::default());
        let other = WindowRegistry::new(DaemonConfig::default());

        assert_ne!(
            registry.boot_nonce(),
            other.boot_nonce(),
            "an approval request id minted by one daemon must never resolve in another"
        );
    }

    #[test]
    fn a_window_id_whose_holder_files_exist_or_that_is_in_use_is_passed_over() {
        let holders_dir =
            yagura_test_support::paths::unique_temp_dir("yagura-test-window-id", "passed-over");
        std::fs::write(holders_dir.join("wrecord01.json"), b"{}").expect("place a record");
        let mut candidates = vec!["wrecord01", "wmemory02"];
        #[cfg(unix)]
        {
            std::fs::write(holders_dir.join("wsocket03.sock"), b"").expect("place a socket");
            std::fs::write(holders_dir.join("wlocked04.sock.lock"), b"").expect("place a lock");
            candidates.extend(["wsocket03", "wlocked04"]);
        }
        candidates.push("wfree0005");

        let picked = first_free_window_id(
            candidates.iter().map(|id| (*id).to_owned()),
            &holders_dir,
            |id| id == "wmemory02",
        );
        assert_eq!(
            picked.as_deref(),
            Some("wfree0005"),
            "an id named by a holder's files, or held by a window in memory, must not be handed out"
        );
        assert_eq!(
            first_free_window_id(["wrecord01".to_owned()], &holders_dir, |_| false),
            None,
            "no id is handed out when every candidate is taken"
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

        let env = child_environment(&config, &token, "w1-test", &[], true);

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
        let session =
            PtySession::spawn_with_command(&argv, &config, &HolderHost::from_config(&config))
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

    #[test]
    fn a_probe_behind_only_whitespace_shows_alone() {
        let behind = ShownText::Body(" \t ~".to_string()).behind('~');
        assert!(
            matches!(behind, Some(BehindProbe::Alone)),
            "whitespace in front of the probe is no body that arrived: {behind:?}"
        );
    }

    // =================================================================
    // Reading the daemon's own probes once a dialog may have taken one.
    // =================================================================

    fn body(text: &str) -> ComposerReadout {
        ComposerReadout::Body(text.to_string())
    }

    fn front_of(counted: Option<(BehindProbe, Vec<char>)>) -> Option<(String, Vec<char>)> {
        counted.map(|(behind, shown)| {
            let front = match behind {
                BehindProbe::Alone => String::new(),
                BehindProbe::Text(ShownText::Body(text)) => format!("body:{text}"),
                BehindProbe::Text(ShownText::Tail(text)) => format!("tail:{text}"),
            };
            (front, shown)
        })
    }

    fn open_reading(readout: ComposerReadout, closed_screens: u64) -> ComposerObservation {
        ComposerObservation {
            readout,
            screen_rule: None,
            painted: true,
            closed_screens,
        }
    }

    fn probes(chars: &str) -> ProbeChars {
        ProbeChars::new(chars).expect("distinct probe characters")
    }

    #[test]
    fn probes_are_not_read_until_the_last_one_typed_shows() {
        assert_eq!(
            front_of(count_own_probes(&body("note x"), &['x', 'z'], None)),
            None,
            "an earlier probe showing says nothing of the later one still on its way"
        );
        assert_eq!(
            front_of(count_own_probes(&ComposerReadout::Empty, &['x'], None)),
            None
        );
    }

    #[test]
    fn probes_are_counted_back_from_the_end_with_any_of_them_missing() {
        assert_eq!(
            front_of(count_own_probes(&body("note xv"), &['x', 'z', 'v'], None)),
            Some(("body:note ".to_string(), vec!['x', 'v'])),
            "a probe a dialog took is skipped, and the count stops at the text"
        );
        assert_eq!(
            front_of(count_own_probes(&body("note z"), &['x', 'z'], None)),
            Some(("body:note ".to_string(), vec!['z']))
        );
        assert_eq!(
            front_of(count_own_probes(&body("note zx"), &['x', 'z'], None)),
            None,
            "probes out of the order typed end with one that was not typed last"
        );
    }

    #[test]
    fn probes_with_nothing_in_front_show_alone() {
        assert_eq!(
            front_of(count_own_probes(&body(" x z"), &['x', 'z'], None)),
            Some((String::new(), vec!['x', 'z']))
        );
    }

    #[test]
    fn a_tail_is_read_only_with_text_in_front_of_the_probes() {
        assert_eq!(
            front_of(count_own_probes(
                &ComposerReadout::Tail("end of it z".to_string()),
                &['x', 'z'],
                None
            )),
            Some(("tail:end of it ".to_string(), vec!['z']))
        );
        assert_eq!(
            front_of(count_own_probes(
                &ComposerReadout::Tail("z".to_string()),
                &['x', 'z'],
                None
            )),
            None,
            "probes alone at the end of a taller composer say nothing of what is above"
        );
    }

    #[test]
    fn a_line_on_its_way_in_front_of_the_probes_is_not_decided() {
        let line = ComposerLine::new("lazy");
        assert_eq!(
            front_of(count_own_probes(&body("lazv"), &['x', 'v'], Some(&line))),
            None,
            "the part of the line drawn so far is no answer"
        );
        assert_eq!(
            front_of(count_own_probes(&body("lazy v"), &['x', 'v'], Some(&line))),
            Some(("body:lazy ".to_string(), vec!['v']))
        );
        assert_eq!(
            front_of(count_own_probes(&body("v"), &['x', 'v'], Some(&line))),
            Some((String::new(), vec!['v'])),
            "the line dropped altogether is an answer"
        );
    }

    #[test]
    fn a_reopened_screen_with_nothing_read_calls_for_the_next_probe_character() {
        let flight = Flight {
            typed: vec!['x'],
            closures: 2,
        };
        let reopened = open_reading(body("note "), 3);
        assert!(matches!(
            probe_step(&reopened, &probes("xzv"), &flight, &['z'], None),
            Some(ProbeStep::Retype('v', 3))
        ));
        assert!(
            probe_step(
                &open_reading(body("note "), 2),
                &probes("xzv"),
                &flight,
                &[],
                None
            )
            .is_none(),
            "with no closed screen since, the probe is waited for"
        );
        let still_closed = ComposerObservation {
            screen_rule: Some(DetectedStatus::WaitingApproval),
            ..reopened.clone()
        };
        assert!(
            probe_step(&still_closed, &probes("xzv"), &flight, &[], None).is_none(),
            "nothing is typed while the dialog is up"
        );
        assert!(matches!(
            probe_step(
                &reopened,
                &probes("xzv"),
                &flight,
                &[],
                Some((BehindProbe::Alone, vec!['x']))
            ),
            Some(ProbeStep::Decided(BehindProbe::Alone, _))
        ));
    }

    #[test]
    fn probes_an_earlier_call_left_past_a_closed_screen_are_undecidable_with_no_other_character() {
        // The record an earlier call left: one probe, typed as of two closed
        // screens. A dialog has come and gone since.
        let flight = Flight {
            typed: vec!['x'],
            closures: 2,
        };
        let reading = open_reading(body("note "), 3);
        let counted = count_own_probes(&reading.readout, &flight.typed, None);
        assert!(matches!(
            probe_step(&reading, &probes("x"), &flight, &[], counted),
            Some(ProbeStep::Undecidable)
        ));
        let every_one_typed = Flight {
            typed: vec!['x', 'z'],
            closures: 2,
        };
        assert!(matches!(
            probe_step(&reading, &probes("xz"), &every_one_typed, &[], None),
            Some(ProbeStep::Undecidable)
        ));
    }

    #[test]
    fn the_probes_behind_a_line_are_split_off_at_its_last_blank() {
        assert_eq!(split_own_probes("note xz"), ("note ", "xz"));
        assert_eq!(split_own_probes("note "), ("note ", ""));
    }
}
