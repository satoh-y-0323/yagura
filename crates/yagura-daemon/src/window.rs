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
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, Notify, broadcast, mpsc, oneshot, watch};
use yagura_os::ancestry::ImagePattern;
use yagura_os::program;
use yagura_protocol::{
    DefaultPalettes, DetectedStatus, INSTANCE_ENV, MCP_BRIDGE_ENV, OnNameTaken, Palette, RoomRef,
    STATUS_TOKEN_ENV, UnknownReason, WINDOW_ID_ENV, WINDOW_NAME_MAX_BYTES, WindowInfo,
    WindowNameError, WindowStatus, WithdrawOutcome, validate_window_name, validate_window_vendor,
};

use crate::config::DaemonConfig;
use crate::detector::definition::{Definition, DefinitionError, is_valid_detection_name};
use crate::detector::engine::{ConfirmedStatus, DetectionEngine, DetectorHandle, InputEcho};
use crate::detector::layers::{DefinitionSource, DetectionLayers, LayerError, ResolvedDefinition};
use crate::holder_host::HolderHost;
use crate::inbox::{Inbox, Sender, is_session_id};
use crate::parcel_store::{InboxKeeper, KeptParcel, MarkKeeper, ParcelStore};
use crate::profile::CredentialsMode;
use crate::profile_delivery::ProfileDelivery;
use crate::pty::{
    AttachGeneration, LostNotice, PtySession, TakeBackSpawn, WindowSpawn, lock_recover,
};
use crate::room::{mention_key, reserved_mention};
use crate::token::StatusToken;
use crate::waiter::StatusAt;
pub use crate::window_table::{HooksRoute, OverlayFact, WindowFacts};
use crate::window_table::{
    OpeningRow, OpeningWrite, PendingWrite, Reservation, RowScopes, RowUnwritten, WindowRow,
    WindowTable, WindowValues, credentials_kind, credentials_mode, launch_record, recorded_launch,
    route_taken_back,
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
/// for a window taken back once its holder returns it. A take-back that ends
/// any other way drops the sender unset, which settles the slot empty.
type TokenSlot = watch::Receiver<Option<Arc<StatusToken>>>;

/// What a holder's `Welcome` gives back for a window taken back without a
/// row: its values, and its token with where to settle it once the window
/// built from them is registered.
struct RowlessWelcome {
    values: WindowValues,
    token: Option<Arc<StatusToken>>,
    token_tx: watch::Sender<Option<Arc<StatusToken>>>,
}

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

/// One round of [`WINDOW_ID_CANDIDATES`] fresh ids: the first that no
/// holder's files in `holders_dir` name and `in_use` does not claim.
fn drawn_window_id(
    holders_dir: Option<&Path>,
    in_use: impl Fn(&str) -> bool,
) -> Result<Option<String>, WindowOpenError> {
    let candidates = (0..WINDOW_ID_CANDIDATES)
        .map(|_| random_window_id())
        .collect::<Result<Vec<_>, _>>()?;
    Ok(match holders_dir {
        Some(dir) => first_free_window_id(candidates, dir, in_use),
        // No holder has been started yet, so none has files.
        None => candidates.into_iter().find(|id| !in_use(id)),
    })
}

/// A fresh window id that no holder's files in `holders_dir` name and `in_use`
/// does not claim, for a window given an id outside an open; `None` when
/// [`WINDOW_ID_ROUNDS`] rounds find none or the random source fails.
pub(crate) fn free_window_id(
    holders_dir: Option<&Path>,
    in_use: impl Fn(&str) -> bool,
) -> Option<String> {
    for _ in 0..WINDOW_ID_ROUNDS {
        match drawn_window_id(holders_dir, &in_use) {
            Ok(Some(window_id)) => return Some(window_id),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "cannot draw a window id");
                return None;
            }
        }
    }
    None
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
        hooks: Some(provenance.hooks),
        facts: provenance.facts.clone(),
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
    /// what the spawned argv really carries ([`HooksRoute::settle`]). A
    /// window taken back has the one its row or launch record kept.
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
    /// What the open leaves beside a `launch` window's credentials file for
    /// its hooks, and where its wake file goes. Empty for a window taken
    /// back: its folder already holds them.
    pub hook_answers: crate::wake_file::HookAnswers,
    /// What the open settled about the window for its whole life; a window
    /// taken back has what its row or launch record kept.
    pub facts: WindowFacts,
    /// How a request in the window's name is traced back to its CLI.
    pub lineage: LineageRule,
    /// The `[launch_overlay]` table of the profile the window was launched
    /// from, which its overlay door's request is built by; `None` when the
    /// profile declares none.
    pub launch_overlay: Option<crate::launch_overlay::LaunchOverlaySpec>,
}

/// How a request in a window's name is traced back to the window's CLI,
/// chosen from what is known of the window, never from the request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LineageRule {
    /// Not traced: the window's credentials and declaration are matched
    /// only. A window whose profile declares no image of its CLI.
    #[default]
    Matched,
    /// Traced by the kernel's process ids from the requester up to the
    /// window's child: exactly one process on the way may run one of these
    /// images.
    ProcessIds(Vec<ImagePattern>),
    /// Traced by what a request collected of its lineage in the WSL guest the
    /// window's CLI runs in, whose processes the kernel's ids do not reach, up
    /// to the root the launch put in the CLI's environment: exactly one
    /// process on the way may run one of these images.
    Guest(Vec<ImagePattern>),
}

impl LineageRule {
    /// The rule for a window whose profile declares `images` for this host
    /// and whose CLI runs in another environment when `runs_in_guest`.
    #[must_use]
    pub fn for_window(images: Vec<ImagePattern>, runs_in_guest: bool) -> Self {
        if images.is_empty() {
            Self::Matched
        } else if runs_in_guest {
            Self::Guest(images)
        } else {
            Self::ProcessIds(images)
        }
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
    /// The `[command]` table's `launch_only`: arguments the CLI takes only
    /// when it starts a window, never when the daemon runs it otherwise.
    LaunchOnly,
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
            Self::LaunchOnly => "launch-only arguments",
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
    /// See [`WindowEntry::delivers`].
    pub delivers: bool,
}

/// What [`WindowRegistry::initialize_detection_for_window`] resolved for a
/// new window: the detector handle and its status stream, the definition's
/// load warning, and what was read out of the definition before it was moved
/// into the detection engine.
struct DetectionSetup {
    handle: Arc<DetectorHandle>,
    detected_rx: mpsc::UnboundedReceiver<ConfirmedStatus>,
    warning: Option<String>,
    /// The name resolution actually used to load this window's definition
    /// (the `detection` argument, or [`DaemonConfig::detection_name`] when it
    /// was omitted), `Some` only when a definition was found this way. This is the file stem [`DetectionLayers::resolve`]
    /// looked up, not the definition's own declared `name` field.
    definition_name: Option<String>,
    /// The loaded definition's `[requests] abandoned_at_turn_end`, read
    /// before the definition moves into the engine, which never hands it
    /// back; `false` without a definition.
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
    /// The daemon is ending every window: an open asked for once that began
    /// is refused, so no window is left running past it.
    #[error("the daemon is shutting down")]
    ShuttingDown,
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
    /// Set by [`WindowEntry::mark_closed`] once `close_window` has succeeded,
    /// which a window whose child merely exited on its own never is.
    closed: AtomicBool,
    /// The turns of the window's CLI, as its hooks report them, and the mark
    /// an AI's interrupt puts on one.
    turn: crate::profile_interrupt::TurnLedger,
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
    /// The confirmed status and its fan-out. Written only
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
    /// See [`WindowEntry::abandons_calls_at_turn_end`].
    abandons_calls_at_turn_end: bool,
    /// Whether a delivery agent is running for this window. The agent itself
    /// lives in the IPC layer and is started through
    /// [`WindowRegistry::wake_notices`].
    agent_running: Mutex<bool>,
    /// How many reported statuses the detector was given, counted under its
    /// lock: tells a status reported later from one reported earlier without
    /// a clock.
    status_events: AtomicU64,
    /// Wakes this window's running delivery agent.
    notice_wake: Notify,
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
    /// The registry's windows, whose inboxes this window's end withdraws what
    /// it sent from. Weak, so an entry never keeps the registry's list alive.
    peers: Weak<Mutex<Vec<Arc<WindowEntry>>>>,
    /// Whether this window's first request refused, or let through
    /// unjudged, by its lineage was logged as a warning.
    lineage_noted: AtomicBool,
    /// Whether the window's launch overlay was made, which its credentials
    /// speak for it only after; `None` when its profile declares none. Starts
    /// as [`WindowFacts::overlay`] and is written with the window's values.
    overlay: Mutex<Option<OverlayFact>>,
    /// Whether a request refused for an overlay not made was logged as a
    /// warning for this window.
    overlay_noted: AtomicBool,
    /// The window's row in the window table, removed before the window's
    /// holder is told to end ([`WindowEntry::remove_row_then`]).
    row: WindowRow,
    /// The registry's [`DaemonConfig::faults`].
    #[cfg(feature = "fault-injection")]
    faults: crate::fault_point::FaultPoints,
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
        self.row.update(|| self.values_now());
    }

    /// [`Self::update_row`], returning what tells once the row took them.
    fn update_row_waited(&self) -> PendingWrite {
        self.row.update_waited(|| self.values_now())
    }

    fn values_now(&self) -> WindowValues {
        self.values_with_overlay(*lock_recover(&self.overlay))
    }

    /// The window's values as they are now, holding `overlay` as whether its
    /// launch overlay was made.
    fn values_with_overlay(&self, overlay: Option<OverlayFact>) -> WindowValues {
        let mut values = WindowValues {
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
        };
        values.facts.overlay = overlay;
        values
    }

    /// Whether the window's credentials wait for its launch overlay: its
    /// profile declares one and it was not made.
    pub(crate) fn overlay_pending(&self) -> bool {
        *lock_recover(&self.overlay) == Some(OverlayFact::Pending)
    }

    /// `true` the first time it is asked: whether a note about this window's
    /// overlay not made is the first, and so a warning.
    pub(crate) fn first_overlay_note(&self) -> bool {
        !self.overlay_noted.swap(true, Ordering::SeqCst)
    }

    /// The `[launch_overlay]` table the window's overlay is built by, when
    /// its profile declares one.
    pub(crate) fn launch_overlay(&self) -> Option<&crate::launch_overlay::LaunchOverlaySpec> {
        self.provenance.launch_overlay.as_ref()
    }

    /// Keeps that the window's launch overlay was made, and returns once its
    /// row holds it: only then may the overlay be handed to the launch, so a
    /// window is never left whose CLI has the overlay but whose next daemon
    /// would refuse it. A row that cannot take it leaves the overlay not
    /// made.
    pub(crate) async fn keep_overlay_made(&self) -> Result<(), RowUnwritten> {
        let before = lock_recover(&self.overlay).replace(OverlayFact::Made);
        let written = self
            .row
            .update_waited(|| self.values_with_overlay(Some(OverlayFact::Made)))
            .written()
            .await;
        if written.is_err() {
            *lock_recover(&self.overlay) = before;
        }
        written
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
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.withdraw_what_it_sent();
        }
    }

    /// Withdraws the envelopes this window sent that still wait in any
    /// window's inbox, once it ended (closed, or its exit published): nobody
    /// is left to answer them. Ones a door carries are left to it.
    fn withdraw_what_it_sent(&self) {
        let Some(peers) = self.peers.upgrade() else {
            return;
        };
        // Copied out first, so no inbox's lock is taken under the list's.
        let peers: Vec<Arc<WindowEntry>> = lock_recover(&peers).clone();
        for peer in peers {
            peer.inbox.withdraw_sent_by(&self.window_id);
        }
    }

    /// Writes `data` to this window's PTY, telling the detector what kind of
    /// output to expect from it.
    ///
    /// The detector is notified *before* the write, so the notification is
    /// queued ahead of the echo it explains. A window whose
    /// detector has already been taken (`Exited`) is written to without a
    /// notification: nothing is left to attribute the output to.
    ///
    /// GUI input goes through [`WindowEntry::write_client_input`] instead.
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
        self.session.write_input(data).await
    }

    /// How many writes this window's child input has taken, from every writer
    /// alike (see [`PtySession::writes_recorded`]).
    #[must_use]
    pub fn write_seq(&self) -> u64 {
        self.session.writes_recorded()
    }

    /// Writes GUI input to this window's PTY, as
    /// [`WindowEntry::write_pty_input`] does.
    ///
    /// # Errors
    ///
    /// As [`WindowEntry::write_pty_input`].
    pub async fn write_client_input(
        &self,
        write: &crate::input_encode::ClientWrite,
        origin: InputOrigin,
    ) -> crate::pty::PtyResult<()> {
        self.write_pty_input(write.bytes(), origin).await
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
    ///
    /// A window being taken back has no token until its holder lets the
    /// daemon in, so this returns `false` meanwhile; see
    /// [`WindowEntry::verify_status_token_once_settled`].
    #[must_use]
    pub fn verify_status_token(&self, candidate: &str) -> bool {
        self.status_token
            .borrow()
            .as_ref()
            .is_some_and(|token| token.verify(candidate))
    }

    /// Whether the window's token is still to come from a take-back in
    /// progress.
    pub(crate) fn token_pending(&self) -> bool {
        self.status_token.borrow().is_none() && self.status_token.has_changed().is_ok()
    }

    /// [`WindowEntry::verify_status_token`] once the window's token is
    /// settled: for a window being taken back, after the take-back ends,
    /// with the token its holder returned, or with none (every candidate
    /// refused) when it failed or the window was closed first. Not bounded
    /// in time, as the take-back is not.
    pub async fn verify_status_token_once_settled(&self, candidate: &str) -> bool {
        let mut slot = self.status_token.clone();
        slot.wait_for(Option::is_some)
            .await
            .is_ok_and(|token| token.as_ref().is_some_and(|token| token.verify(candidate)))
    }

    /// The parcels waiting for this window's CLI.
    pub fn inbox(&self) -> &Inbox {
        &self.inbox
    }

    /// The turns of the window's CLI and the mark an AI's interrupt put on
    /// one.
    pub fn turn(&self) -> &crate::profile_interrupt::TurnLedger {
        &self.turn
    }

    /// The point this window's daemon has armed.
    #[cfg(feature = "fault-injection")]
    pub(crate) fn faults(&self) -> &crate::fault_point::FaultPoints {
        &self.faults
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

    /// The profile a hook of this window, and a bridge claiming it, must
    /// name: the one it was launched from, once the daemon arranged its
    /// hooks. `None` for a window it arranged none for (a raw argv, a shell),
    /// which takes what any CLI started in it names.
    pub(crate) fn named_profile(&self) -> Option<&str> {
        if self.provenance.hooks == HooksRoute::Unarranged {
            return None;
        }
        self.launch_profile()
    }

    /// The profile the window was launched from, whatever became of its
    /// hooks; `None` for a window opened from a raw argv.
    pub(crate) fn launch_profile(&self) -> Option<&str> {
        self.provenance
            .launch
            .as_ref()
            .map(|launch| launch.profile.as_str())
    }

    /// How a request in the window's name is traced back to its CLI.
    pub(crate) fn lineage_rule(&self) -> &LineageRule {
        &self.provenance.lineage
    }

    /// `true` the first time it is asked: whether a lineage note about this
    /// window is the first, and so a warning.
    pub(crate) fn first_lineage_note(&self) -> bool {
        !self.lineage_noted.swap(true, Ordering::SeqCst)
    }

    /// Where the daemon writes to ask the window's CLI to wait again, as the
    /// window's open decided it; `None` for a window that cannot be asked.
    #[must_use]
    pub fn wake_record(&self) -> Option<&crate::wake_file::WakeRecord> {
        self.provenance.facts.wake.as_ref()
    }

    /// The times the window's CLI gives its hooks, as the profile it was
    /// opened from declared them then; `None` for a window that keeps none.
    #[must_use]
    pub fn hook_timeouts(&self) -> Option<&crate::profile::HookTimeouts> {
        self.provenance.facts.hook_timeouts.as_ref()
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
        self.record_status_event_if(status, |_| true)
    }

    /// How many reported statuses this window's detector has been given; see
    /// [`WindowEntry::record_status_event_unless_newer`].
    #[must_use]
    pub fn status_event_count(&self) -> u64 {
        self.status_events.load(Ordering::SeqCst)
    }

    /// Records `status` as [`WindowEntry::record_status_event`] does, but
    /// only when no status was reported since
    /// [`WindowEntry::status_event_count`] read `seen`. Returns whether it was
    /// recorded.
    pub fn record_status_event_unless_newer(&self, status: WindowStatus, seen: u64) -> bool {
        self.record_status_event_if(status, |count| count == seen)
    }

    fn record_status_event_if(&self, status: WindowStatus, when: impl FnOnce(u64) -> bool) -> bool {
        // The count is read and moved under the detector's lock, so no report
        // falls between the check and the record.
        let detector = lock_recover(&self.detector);
        match detector.as_ref() {
            Some(detector) if when(self.status_events.load(Ordering::SeqCst)) => {
                self.status_events.fetch_add(1, Ordering::SeqCst);
                detector.record_status_event(status);
                true
            }
            _ => false,
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
            self.withdraw_what_it_sent();
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

    /// The vendor label this window was opened with: the id of the profile it
    /// was launched from, when it was.
    #[must_use]
    pub fn vendor(&self) -> Option<&str> {
        self.vendor.as_deref()
    }

    /// Stores `name` and returns the value as stored, with the write of it to
    /// the row. Reached only through [`WindowRegistry::rename_window`], which
    /// has already judged the name.
    fn store_name(&self, name: &str) -> (String, PendingWrite) {
        let stored = {
            let mut stored = lock_recover(&self.name);
            name.clone_into(&mut stored);
            stored.clone()
        };
        self.participation.raise();
        (stored, self.update_row_waited())
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
            queued_count: Some(u64::try_from(self.inbox.pending_count()).unwrap_or(u64::MAX)),
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

/// [`WindowRegistry::add_kept_parcels_hook`].
pub(crate) type KeptParcelsHook = Arc<dyn Fn(&Arc<WindowEntry>) + Send + Sync>;

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
    entries: Arc<Mutex<Vec<Arc<WindowEntry>>>>,
    /// Serializes the whole of [`WindowRegistry::open`] (limit check ->
    /// spawn -> insert) against concurrent opens, so the dual resource
    /// limits are enforced atomically rather than raced past.
    open_lock: AsyncMutex<()>,
    /// What every window's holder is started in.
    holder_host: Arc<HolderHost>,
    /// The table every window's row goes in, installed once at start-up by
    /// `store::wire_into`; `None` when the registry has no database.
    window_table: Mutex<Option<WindowTable>>,
    /// Where every window's inbox keeps its parcels, installed with the window
    /// table; `None` when the registry has no database.
    parcel_store: Mutex<Option<ParcelStore>>,
    /// The one `StatusEvent` auth token every window must adopt, when
    /// [`DaemonConfig::status_token_override`] supplied a well-formed value.
    /// `None` -- the production setting -- means each window gets its own
    /// freshly generated token instead, which is what keeps one window's child
    /// from being able to speak for another.
    status_token_override: Option<Arc<StatusToken>>,
    /// Called at the end of every successful `open`, once the
    /// new entry is already registered and can be looked up by id. `None`
    /// when chat is disabled or this registry predates chat entirely (most
    /// tests): a registry never knows what a chat participant is on its own.
    window_opened_hook: Mutex<Option<WindowOpenedHook>>,
    /// Answer for what the store kept for a window taken back, whose
    /// depositors are gone, as each entry of it is built. None until an IPC
    /// server is built on this registry.
    kept_parcels_hooks: Mutex<Vec<KeptParcelsHook>>,
    /// Starts or wakes a window's notice delivery agent. `None` until an IPC
    /// server is built on this registry: notices queued before then wait.
    notice_launcher: Mutex<Option<NoticeLauncher>>,
    /// Caps concurrent workspace-key walks: they block and cannot be
    /// aborted, so an unbounded number would drain the shared blocking pool.
    workspace_walks: tokio::sync::Semaphore,
    /// Serializes writing MCP settings files, so two opens needing the same
    /// file never race on one temporary file name.
    mcp_config_write: AsyncMutex<()>,
    /// The guest wake slots this daemon's windows hold.
    wake_slots: crate::wake_file::WakeSlots,
    /// Bounds the wake I/O of the opens and of the delivery agents together.
    wake_io: crate::wake_file::WakeIo,
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
        // The bound of the state base tool an open runs before it.
        let wake_io = crate::wake_file::WakeIo::new(config.mcp_listing_timeout);

        Self {
            config,
            entries: Arc::new(Mutex::new(Vec::new())),
            open_lock: AsyncMutex::new(()),
            holder_host,
            window_table: Mutex::new(None),
            parcel_store: Mutex::new(None),
            status_token_override,
            window_opened_hook: Mutex::new(None),
            kept_parcels_hooks: Mutex::new(Vec::new()),
            notice_launcher: Mutex::new(None),
            workspace_walks,
            mcp_config_write: AsyncMutex::new(()),
            wake_slots: crate::wake_file::WakeSlots::default(),
            wake_io,
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
        mut provenance: WindowProvenance,
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
        // Before the window's values are taken from the facts, and before its
        // CLI can run a hook asking for an answer.
        let answers = std::mem::take(&mut provenance.hook_answers);
        provenance.facts.wake = match credentials_folder.as_ref() {
            Some(folder) => self.leave_answers(folder, answers).await,
            None => None,
        };

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
        // Under the lock `terminate_all` raises the flag in, so an open either
        // is among those it waits for or sees the flag here.
        if self.is_shutting_down() {
            return Err(WindowOpenError::ShuttingDown);
        }
        self.enforce_limits_before_open()?;
        let name = {
            let entries = lock_recover(&self.entries);
            let opening = lock_recover(&self.opening);
            claim_name(&entries, &opening, &name, on_name_taken)?
        };

        // The id and token exist before the spawn, so they can be injected
        // into the child environment. The route is the spawn's to settle, so
        // the `opening` row carries none.
        let values = WindowValues {
            hooks: None,
            ..window_values(
                &provenance,
                &name,
                &cwd_string,
                vendor.as_ref(),
                &command,
                detection.as_ref(),
            )
        };
        let (window_id, row) = self.reserve_window_id(&values).await?;
        let status_token = self.acquire_status_token();
        let opening = self.begin_opening(&window_id, &name);
        drop(open_guard);

        let palette = self.default_palette_for(vendor.as_deref());
        let settled = |hooks: HooksRoute| WindowValues {
            hooks: Some(hooks),
            facts: values.facts.clone().settled(hooks),
            ..values.clone()
        };
        let writer = row.writer();
        let lost = row.lost_notice();
        let scopes = row.scopes();
        let ((session, hooks), row) = row
            .open(self.spawn_pty_for_window(
                &command,
                &resolved_cwd,
                &status_token,
                &window_id,
                (settled, writer, lost, scopes),
                &resolution,
                extra_profile_env,
                &prepared,
                palette,
                (provenance.hooks, provenance.credentials),
                credentials_folder.as_ref(),
            ))
            .await?;
        #[cfg(feature = "fault-injection")]
        self.config
            .faults
            .reach(crate::fault_point::FaultPoint::WindowHolderStarted);
        #[cfg(feature = "fault-injection")]
        self.config.faults.hold_registration().await;
        let session = Arc::new(session);
        if let Some(folder) = credentials_folder {
            remove_credentials_when_exited(&session, folder);
        }

        let DetectionSetup {
            handle: detector_handle,
            detected_rx,
            warning: detection_warning,
            definition_name,
            abandons_calls_at_turn_end,
        } = self.initialize_detection_for_window(&window_id, &session, detection.as_deref());
        // Settled where the injection is finally carried out, so hooks that
        // were dropped on the way (gate, file, shim) neither expect reports
        // nor take part in delivery.
        let provenance = WindowProvenance {
            hooks,
            facts: provenance.facts.clone().settled(hooks),
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
            watch::channel(Some(status_token)).1,
            abandons_calls_at_turn_end,
            definition_name,
            detection_warning.clone(),
            provenance,
            row,
            Vec::new(),
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
        let (stored, write) = {
            let _open_guard = self.open_lock.lock().await;
            let refusal = {
                let entries = lock_recover(&self.entries);
                name_refusal(&entries, &lock_recover(&self.opening), name, Some(entry))
            };
            if let Some(refusal) = refusal {
                return Err(refusal.into());
            }
            entry.store_name(name)
        };
        // Answered once the row has the name: no launch record carries it, so
        // a name only the daemon held would be gone after a crash. A row that
        // cannot take it keeps the rename, as a window without a row does.
        let _ = write.written().await;
        Ok(stored)
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

    /// Leaves `answers` beside the credentials file in `folder`, with the wake
    /// record they name; returns the record, `None` when the window has none
    /// or the answers cannot be written (its CLI then learns of no wake file).
    async fn leave_answers(
        &self,
        folder: &CredentialsFolder,
        answers: crate::wake_file::HookAnswers,
    ) -> Option<crate::wake_file::WakeRecord> {
        let crate::wake_file::HookAnswers { texts, wake } = answers;
        let record = match wake.and_then(|place| self.wake_slots.record(&place, &folder.dir)) {
            // A credentials folder is there already; a guest slot's folder
            // must be before the CLI starts watching in it.
            Some((record, Some(slot))) => match self.wake_io.make_slot_folder(&record).await {
                Ok(()) => {
                    folder.wake.keep(slot);
                    Some(record)
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "the guest's wake folder could not be made; the window opens without a \
                         wake file, so a message for it waits for its CLI's next event"
                    );
                    None
                }
            },
            Some((record, None)) => Some(record),
            None => None,
        };
        if texts.is_empty() {
            return record;
        }
        let rendered = crate::wake_file::rendered_answers(&texts, record.as_ref());
        let dir = folder.dir.clone();
        let written =
            tokio::task::spawn_blocking(move || crate::wake_file::write_answers(&dir, &rendered))
                .await
                .unwrap_or_else(|err| Err(std::io::Error::other(err.to_string())));
        match written {
            Ok(()) => record,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "could not write the answers of the window's hooks; the window opens without \
                     a wake file"
                );
                None
            }
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
    /// holder keeps the launch record `record` builds from the route `hooks`
    /// settles to in memory only; the settled route is returned with the
    /// session.
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
        (settled, writer, lost, scopes): (
            impl FnOnce(HooksRoute) -> WindowValues,
            OpeningWrite,
            LostNotice,
            Option<RowScopes>,
        ),
        resolution: &program::Resolution,
        extra_profile_env: &[(String, String)],
        injections: &[PreparedInjection],
        palette: Palette,
        (hooks, credentials): (HooksRoute, CredentialsMode),
        credentials_folder: Option<&CredentialsFolder>,
    ) -> Result<(PtySession, HooksRoute), WindowOpenError> {
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
        let hooks = hooks.settle(hooks_injected);
        let settled = settled(hooks);
        // Written before the holder starts: a daemon that ends once the holder
        // runs leaves a row the next daemon takes the window back from, route
        // and all. A row that cannot take it refuses the open, as no id could.
        writer.write(settled.clone()).await.map_err(|_| {
            WindowOpenError::SpawnFailed("the window's row could not be written".to_owned())
        })?;

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
                record: launch_record(window_id, status_token.as_hex(), &settled),
                lost,
                scopes,
            },
            &self.config,
            &self.holder_host,
        )
        .await
        .map_err(|err| WindowOpenError::SpawnFailed(err.to_string()))?;
        Ok((session, hooks))
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
    /// What the window keeps of the definition is read *before* the
    /// definition is moved into [`DetectionEngine::new`] -- once moved,
    /// nothing downstream of the engine can see it again.
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
            definition_name,
            abandons_calls_at_turn_end,
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
            let in_use: HashSet<String> = {
                let entries = lock_recover(&self.entries);
                let opening = lock_recover(&self.opening);
                entries
                    .iter()
                    .map(|entry| entry.window_id.clone())
                    .chain(opening.keys().cloned())
                    .collect()
            };
            let picked = drawn_window_id(holders_dir.as_deref(), |id| in_use.contains(id))?;
            let Some(window_id) = picked else {
                continue;
            };
            // Named for each id tried: a name is never used for two starts.
            let scopes = self.holder_host.scope_names(&window_id);
            if let Reservation::Reserved(row) =
                OpeningRow::reserve(table.as_ref(), &window_id, values, scopes).await
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
        abandons_calls_at_turn_end: bool,
        detection_definition: Option<String>,
        detection_warning: Option<String>,
        provenance: WindowProvenance,
        row: WindowRow,
        kept: Vec<KeptParcel>,
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
        let status_gate = StatusGate::new(initial_status, tokio::time::Instant::now());
        let delivery_spec = provenance
            .delivery
            .as_ref()
            .map_or_else(|| self.config().delivery.clone(), ProfileDelivery::spec);
        let keeper = self
            .parcel_store()
            .map(|store| InboxKeeper::new(store, &window_id));
        let inbox = Inbox::kept(
            self.config().inbox_capacity,
            self.config().max_queued_per_sender,
            delivery_spec,
            // The registry does not know whether the room introduces this
            // window, so the limit counts the configured intro.
            &self.config().chat_intro,
            status_gate.watch(),
            keeper,
            kept,
        );
        let detector_weak = Arc::downgrade(&detector_handle);
        let provenance_overlay = provenance.facts.overlay;

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
            turn: crate::profile_interrupt::TurnLedger::default(),
            session,
            detector: Mutex::new(Some(detector_handle)),
            detector_weak,
            status_gate,
            status_token,
            abandons_calls_at_turn_end,
            agent_running: Mutex::new(false),
            status_events: AtomicU64::new(0),
            notice_wake: Notify::new(),
            inbox,
            session_id: Mutex::new(None),
            provenance,
            mcp_connections: Arc::new(AtomicUsize::new(0)),
            participation: Arc::clone(&self.participation),
            peers: Arc::downgrade(&self.entries),
            lineage_noted: AtomicBool::new(false),
            overlay: Mutex::new(provenance_overlay),
            overlay_noted: AtomicBool::new(false),
            row,
            #[cfg(feature = "fault-injection")]
            faults: self.config().faults.clone(),
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

    /// Withdraws the envelope `id` for `asker` from whichever live window's
    /// inbox holds it ([`Inbox::withdraw_sent`]). An envelope no live window
    /// holds, or that `asker` did not send, is not waiting.
    pub fn withdraw_sent(&self, id: &str, asker: &Sender) -> WithdrawOutcome {
        let entries: Vec<Arc<WindowEntry>> = lock_recover(&self.entries).clone();
        entries
            .iter()
            .filter(|entry| matches!(self.liveness(&entry.window_id), WindowLiveness::Alive(_)))
            .map(|entry| entry.inbox.withdraw_sent(id, asker))
            .find(|outcome| *outcome != WithdrawOutcome::NotWaiting)
            .unwrap_or(WithdrawOutcome::NotWaiting)
    }

    /// Installs the window table every window opened from now on writes its
    /// row in (`store::wire_into` is its only caller).
    pub(crate) fn set_window_table(&self, table: WindowTable) {
        #[cfg(feature = "fault-injection")]
        self.config.faults.attach_window_table(&table);
        *lock_recover(&self.window_table) = Some(table);
    }

    /// Has the writer of the window table installed now act out `faults`
    /// from its next command on, so a test arms it once its windows are
    /// open. Nothing without a table. Behind the `fault-injection` feature
    /// only.
    #[cfg(feature = "fault-injection")]
    pub fn arm_window_table(&self, faults: &crate::fault_point::FaultPoints) {
        if let Some(table) = self.window_table() {
            faults.attach_window_table(&table);
        }
    }

    /// Installs where every window opened or taken back from now on keeps its
    /// inbox's parcels (`store::wire_into` is its only caller).
    pub(crate) fn set_parcel_store(&self, store: ParcelStore) {
        #[cfg(feature = "fault-injection")]
        self.config.faults.attach(&store);
        *lock_recover(&self.parcel_store) = Some(store);
    }

    /// Where every window's parcels are kept; `None` when the registry has no
    /// database.
    pub(crate) fn parcel_store(&self) -> Option<ParcelStore> {
        lock_recover(&self.parcel_store).clone()
    }

    /// The table every window's row goes in; `None` when the registry has no
    /// database.
    pub(crate) fn window_table(&self) -> Option<WindowTable> {
        lock_recover(&self.window_table).clone()
    }

    /// Where the holders that outlive this daemon are, and what they are
    /// taken back with; `None` when this registry's holders end with it.
    pub(crate) fn take_back_context(&self) -> Option<crate::holder_host::TakeBackContext> {
        self.holder_host.take_back_context()
    }

    /// The directory this registry's holders keep their files in; `None`
    /// before the first holder of a registry whose holders end with it.
    pub(crate) fn holders_dir(&self) -> Option<PathBuf> {
        self.holder_host.holders_dir()
    }

    /// Registers the window `survivor` names, which a previous daemon of the
    /// instance left running, under its id: not connected and `Unknown`,
    /// listed, closable and stopped with the daemon from now on. Its link
    /// lets itself into the holder once `begin` turns `true`; the holder's
    /// launch record then gives back the window's token. A window with a row
    /// takes the values the row keeps; one without takes only its id, and the
    /// values the launch record keeps (with its row written again) once the
    /// holder lets the daemon in. A row without the route its hooks take
    /// waits the same way, for the route alone. Not refused by the window
    /// limits: the window already exists. The chat introduction is not sent
    /// again.
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
            credentials,
        } = survivor;
        let (token_tx, status_token) = watch::channel(None);
        let (recorded_tx, recorded_rx) = oneshot::channel::<RowlessWelcome>();
        // A row whose values do not read back counts as none: the launch
        // record gives them back, and the row is written again from them. A
        // row without the route its hooks take (written before the spawn
        // settled it, or before the table kept it) keeps its other values,
        // and takes the route from the launch record.
        let stored = stored.and_then(|stored| stored.values);
        let needs_values = stored.as_ref().is_none_or(|values| values.hooks.is_none());
        let on_welcome: Box<dyn FnOnce(&str) + Send> = {
            let window_id = window_id.clone();
            let kept = stored.clone().filter(|_| needs_values);
            Box::new(move |record: &str| {
                let recorded = recorded_launch(record, &window_id);
                let token = recorded
                    .as_ref()
                    .and_then(|recorded| recorded.token.as_deref())
                    .and_then(StatusToken::from_override)
                    .map(Arc::new);
                if token.is_none() {
                    tracing::warn!(
                        window_id,
                        "the holder's launch record carries no token for the window; its status events are refused"
                    );
                }
                let values = match kept {
                    Some(kept) => {
                        let recorded = recorded.map(|recorded| recorded.values);
                        Some(WindowValues {
                            hooks: recorded.as_ref().and_then(|recorded| recorded.hooks),
                            facts: match recorded {
                                Some(recorded) => kept.facts.clone().or(recorded.facts),
                                None => kept.facts.clone(),
                            },
                            ..kept
                        })
                    }
                    None => recorded
                        .filter(|_| needs_values)
                        .map(|recorded| recorded.values),
                };
                match values {
                    // The token waits for the window built from the values, so
                    // a request checked against it is served by that window.
                    Some(values) => {
                        let _ = recorded_tx.send(RowlessWelcome {
                            values,
                            token,
                            token_tx,
                        });
                    }
                    None => {
                        if let Some(token) = token {
                            token_tx.send_replace(Some(token));
                        }
                    }
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
            Some(values) if !needs_values => (
                values,
                WindowRow::taken_back(self.window_table(), &window_id),
                true,
            ),
            // The command decides how the session encodes keys, which the
            // window built from the values goes on to use.
            kept => (
                WindowValues {
                    command: kept.map(|kept| kept.command).unwrap_or_default(),
                    ..unknown_values(&window_id)
                },
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
        // The folder goes once the window ends, as for a window opened here:
        // closed, its child exited, or its holder found gone. Its wake slot
        // goes with it, held again from the values kept.
        let wake = credentials.map(|dir| {
            let wake = crate::wake_file::WakeHold::default();
            remove_credentials_when_exited(
                &session,
                CredentialsFolder {
                    dir,
                    wake: wake.clone(),
                },
            );
            wake
        });
        if !needs_values {
            self.hold_wake_slot(wake.as_ref(), &values.facts);
        }
        let (entry, detector_handle, detected_rx) = self
            .taken_back_entry(&window_id, values, known, &session, status_token, row)
            .await?;
        self.register_window(&session, entry.clone(), detector_handle, detected_rx, None);
        if needs_values {
            let registry = Arc::downgrade(self);
            tokio::spawn(async move {
                let Ok(welcome) = recorded_rx.await else {
                    return;
                };
                let Some(registry) = registry.upgrade() else {
                    return;
                };
                registry.hold_wake_slot(wake.as_ref(), &welcome.values.facts);
                if registry.fill_taken_back(&entry, welcome.values).await
                    && let Some(token) = welcome.token
                {
                    welcome.token_tx.send_replace(Some(token));
                }
            });
        }
        Ok(())
    }

    /// Holds again, in `hold`, the wake slot a window taken back keeps in
    /// `facts`; a hold already given back with its folder gives it back at
    /// once.
    fn hold_wake_slot(&self, hold: Option<&crate::wake_file::WakeHold>, facts: &WindowFacts) {
        if let (Some(hold), Some(id)) = (
            hold,
            facts.wake.as_ref().and_then(|wake| wake.slot.as_ref()),
        ) {
            hold.keep(self.wake_slots.hold(id));
        }
    }

    /// Replaces `placeholder`, a window taken back without a row, with the
    /// same window built from `values`, which its holder's launch record kept,
    /// and writes its row again. A window closed or removed meanwhile is left
    /// so, and `false` is returned; the placeholder stays when the window
    /// cannot be built.
    async fn fill_taken_back(&self, placeholder: &Arc<WindowEntry>, values: WindowValues) -> bool {
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
                placeholder.status_token.clone(),
                row,
            )
            .await;
        let (entry, detector_handle, detected_rx) = match built {
            Ok(built) => built,
            Err(error) => {
                tracing::warn!(window_id, %error, "cannot give the window taken back its values");
                return true;
            }
        };
        {
            let mut entries = lock_recover(&self.entries);
            let Some(slot) = entries
                .iter_mut()
                .find(|entry| Arc::ptr_eq(entry, placeholder))
            else {
                return false;
            };
            if placeholder.closed.load(Ordering::SeqCst) {
                return false;
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
        true
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
            definition_name,
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
            hooks: route_taken_back(values.hooks),
            wake_command: profile.wake_command,
            credentials: credentials_mode(values.credentials),
            // Only an open creates the folder; a window taken back has its own.
            credentials_dir: None,
            hook_answers: crate::wake_file::HookAnswers::default(),
            // A row or record older than the fact: the window is taken to run
            // in another environment when its profile converts paths for one.
            lineage: LineageRule::for_window(
                profile.process_image,
                values.facts.runs_in_guest.unwrap_or(profile.host_paths),
            ),
            // A row or record older than the fact, of a window whose profile
            // now declares an overlay: its CLI may hold the credentials in its
            // tools, so they do not speak for it.
            facts: WindowFacts {
                overlay: values.facts.overlay.or(profile
                    .launch_overlay
                    .as_ref()
                    .map(|_| OverlayFact::Pending)),
                ..values.facts.clone()
            },
            launch_overlay: profile.launch_overlay,
        };
        let overlay_before_fact =
            values.facts.overlay.is_none() && provenance.facts.overlay.is_some();
        if provenance.hooks.reaches() {
            detector_handle.expect_status_reports(true);
        }
        spawn_output_pump(session, Arc::downgrade(&detector_handle));
        // A placeholder answers for no delivery, so its parcels wait in the
        // store for the window built from its values.
        let kept = match self.parcel_store().filter(|_| known) {
            Some(store) => store.load(window_id).await,
            None => Vec::new(),
        };
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
            abandons_calls_at_turn_end,
            definition_name,
            detection_warning,
            provenance,
            row,
            kept,
        );
        if overlay_before_fact && entry.first_overlay_note() {
            tracing::warn!(
                window_id,
                "the window was opened by a version that did not make its launch overlay, so its \
                 credentials do not speak for it; open the window again"
            );
        }
        if let Some(layout_json) = values.layout_json {
            entry.set_layout(layout_json);
        }
        if let Some(session_id) = values.session_id.as_deref() {
            entry.record_session_id(session_id);
        }
        if let Some(store) = self.parcel_store() {
            drop_typing_marks(MarkKeeper::new(store, window_id)).await;
        }
        let kept_parcels_hooks = lock_recover(&self.kept_parcels_hooks).clone();
        for hook in kept_parcels_hooks {
            hook(&entry);
        }
        Ok((entry, detector_handle, detected_rx))
    }

    /// This registry's own configuration, for a caller that needs it outside
    /// this module (the chat intro hook reads `handoff_ready_timeout_secs`
    /// the same way a delivery does).
    pub(crate) fn config(&self) -> &DaemonConfig {
        &self.config
    }

    /// The bound on wake I/O the opens of this registry's windows count
    /// against, for the writes that ask their CLIs to wait.
    pub(crate) fn wake_io(&self) -> &crate::wake_file::WakeIo {
        &self.wake_io
    }

    /// Installs the hook called at the end of every successful `open`
    /// (`chat::install_intro_hook` is its only caller). Chat
    /// being disabled, or never wired at all, is represented by never calling
    /// this, so `open_impl` stays a no-op past registration in that case.
    pub fn set_window_opened_hook(&self, hook: WindowOpenedHook) {
        *lock_recover(&self.window_opened_hook) = Some(hook);
    }

    /// Adds `hook`, which answers for what the store kept for a window taken
    /// back, and runs it for every window registered so far; every entry of a
    /// window taken back from then on runs it as it is built, before it can
    /// be reached.
    pub(crate) fn add_kept_parcels_hook(&self, hook: KeptParcelsHook) {
        lock_recover(&self.kept_parcels_hooks).push(Arc::clone(&hook));
        let entries: Vec<Arc<WindowEntry>> = lock_recover(&self.entries).clone();
        for entry in &entries {
            hook(entry);
        }
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
                delivers: entry.delivers(),
            })
            .collect()
    }

    /// Records `layout_json` as `entry`'s presentation layout, which its row
    /// follows.
    pub fn set_window_layout(&self, entry: &WindowEntry, layout_json: String) {
        entry.set_layout(layout_json);
    }

    /// Terminates every window's child process, then waits, at most
    /// `close_grace_ms` for all of them together, for every window's holder
    /// process to end. Statuses converge to `Exited`
    /// asynchronously as each session's exit is observed; this method does
    /// not wait for that convergence.
    ///
    /// An open already past its binding decision is waited for first, so its
    /// window is ended too; an open that reaches the decision afterwards is
    /// refused with [`WindowOpenError::ShuttingDown`].
    pub async fn terminate_all(&self) {
        let opening: Vec<watch::Receiver<()>> = {
            let _open_guard = self.open_lock.lock().await;
            self.shutting_down.store(true, Ordering::SeqCst);
            lock_recover(&self.opening)
                .values()
                .map(|open| open.released.clone())
                .collect()
        };
        for mut released in opening {
            // Nothing is ever sent, so this returns only once the open ends;
            // the open's own holder start bounds it.
            let _closed = released.changed().await;
        }
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
        hooks: None,
        facts: WindowFacts::default(),
    }
}

/// What a window's profile determines and its row does not keep.
#[derive(Default)]
struct ProfileDetermined {
    delivery: Option<ProfileDelivery>,
    wake_command: Option<WakeCommand>,
    /// The images of its CLI the profile declares for this host.
    process_image: Vec<ImagePattern>,
    /// Whether the profile converts paths for a CLI in another environment.
    host_paths: bool,
    /// The profile's `[launch_overlay]` table.
    launch_overlay: Option<crate::launch_overlay::LaunchOverlaySpec>,
}

/// What the profile `profile` determines for a window taken back, resolved
/// against the profiles this daemon loaded the way an open plans it: its
/// `[delivery]` and its wake command. The defaults, with one line logged, when
/// the profile cannot be launched any more. The route its hooks take is not
/// planned again: the window's row and launch record keep the one its open
/// settled ([`route_taken_back`]).
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
                "the profile of a window taken back cannot be resolved; its delivery and wake command take the defaults"
            );
            return ProfileDetermined::default();
        }
    };
    let wake_command = launch
        .delivery
        .as_ref()
        .filter(|delivery| matches!(delivery.wake, crate::profile_delivery::WakeMode::Command(_)))
        .map(|_| WakeCommand {
            argv: crate::profile::without_door(launch.argv.clone()),
            env: launch.env.clone().into_iter().collect(),
        });
    ProfileDetermined {
        delivery: launch.delivery,
        wake_command,
        process_image: launch.process_image,
        host_paths: launch.host_paths.is_some(),
        launch_overlay: launch.launch_overlay,
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
    /// The wake slot the window holds, given back with the folder.
    wake: crate::wake_file::WakeHold,
}

impl CredentialsFolder {
    /// Blocking: makes the folder `dir` (from [`plan_credentials_folder`]),
    /// owner-only.
    fn create_at(dir: PathBuf) -> std::io::Result<Self> {
        // Owns the path before anything exists, so a partial creation is cleared.
        let folder = Self {
            dir,
            wake: crate::wake_file::WakeHold::default(),
        };
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
    folder_file_argument(dir, CREDENTIALS_FILE_NAME)
}

/// The path of the file `file_name` in the folder `dir`, named as
/// [`credentials_file_argument`] names the credentials file.
pub(crate) fn folder_file_argument(dir: &Path, file_name: &str) -> Option<String> {
    let path = dir.join(file_name).to_str()?.to_string();
    let path = if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path
    };
    (!path.contains(['\'', '"', '\\']) && !path.chars().any(char::is_control)).then_some(path)
}

impl Drop for CredentialsFolder {
    fn drop(&mut self) {
        // Before the folder goes: a window opened once it is gone may take
        // the slot again.
        self.wake.release();
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

/// Blocking: removes the credentials folders earlier daemon runs of this
/// instance left in `config_dir`, but those of the windows `remaining` keeps
/// the credentials of. Run once the instance's endpoint is held and the start
/// has judged its windows, so no window that may still run loses its file.
pub fn remove_stale_credentials(config_dir: &Path, remaining: &crate::recovery::Remaining) {
    for (dir, window_id) in credentials_folders(config_dir) {
        let kept = window_id.is_some_and(|window_id| remaining.keeps_credentials_of(&window_id));
        if !kept && let Err(err) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(error = %err, "could not remove a stale credentials folder");
        }
    }
}

/// Blocking: every credentials folder in `config_dir` (never a link), with the
/// window its file names, if it names one.
pub(crate) fn credentials_folders(config_dir: &Path) -> Vec<(PathBuf, Option<String>)> {
    let Ok(entries) = std::fs::read_dir(config_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(CREDENTIALS_FOLDER_PREFIX))
                && entry.file_type().is_ok_and(|kind| kind.is_dir())
        })
        .map(|entry| {
            let dir = entry.path();
            let window_id = credentials_window_id(&dir);
            (dir, window_id)
        })
        .collect()
}

/// Blocking: the window the credentials file in the folder `dir` names;
/// `None` when it is not a plain file of that shape.
fn credentials_window_id(dir: &Path) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Named {
        window_id: String,
    }
    let path = dir.join(CREDENTIALS_FILE_NAME);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let body = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str::<Named>(&body)
        .ok()
        .map(|named| named.window_id)
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

/// Forgets the delivery marks a daemon that typed into windows left for one,
/// without taking anything out of its composer: nothing is typed into a
/// window any more, so a marked body there is the user's to keep.
async fn drop_typing_marks(marks: MarkKeeper) {
    let bodies = marks.marks().await;
    if bodies.is_empty() {
        return;
    }
    tracing::warn!(
        count = bodies.len(),
        "dropped the delivery marks an older daemon left for a window; nothing is taken back \
         out of its composer"
    );
    for body in &bodies {
        marks.unmark(body);
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
        | DefinitionError::TooManyRules { .. } => summarize_definition_load_failure(rendered),
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
        let configured = crate::config::APPROVAL_TIMEOUT_DEFAULT_SECS + 1;
        let registry = WindowRegistry::new(DaemonConfig {
            approval_timeout_secs: configured,
            ..DaemonConfig::default()
        });
        assert_eq!(
            registry.config().approval_timeout_secs,
            configured,
            "a reader of the playbook's settings reads them through config()"
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
}
