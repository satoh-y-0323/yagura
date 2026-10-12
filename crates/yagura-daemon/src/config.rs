//! Daemon configuration for the PTY sessions and the IPC server.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use yagura_os::InstanceNamespace;
use yagura_os::program::ProgramLookup;

use crate::child_env::ChildBaseEnv;
use crate::control::ApprovalPolicy;
use crate::profile::ProfileOs;
use crate::room;

/// How `interrupt_window` delivers an interrupt to a window.
///
/// The method is configuration rather than a per-platform constant: which of
/// the two a given CLI reacts to depends on how it reads its console input,
/// so an operator can select it through a playbook's `[limits]` table
/// (`interrupt_method = "bytes" | "signal"`).
///
/// Selecting a method this build cannot carry out is refused rather than
/// silently served by the other one: [`InterruptMethod::Signal`] currently
/// answers `ControlError::UnsupportedInterruptMethod`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptMethod {
    /// Write [`DaemonConfig::interrupt_bytes`] into the window's PTY.
    Bytes,
    /// Send an interrupt signal to the window's process group. Not implemented
    /// on any platform yet.
    Signal,
}

/// Detection definition selected when nothing overrides it. Defined here
/// only; every other layer receives the value through
/// [`DaemonConfig::detection_name`].
const DEFAULT_DETECTION_NAME: &str = "generic-shell";

/// Configuration for PtySession and IpcServer.
///
/// Injected into both so that timeouts and buffer sizes can be set from a
/// test rather than being compiled in.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Milliseconds a raw detection judgement must hold before it becomes the
    /// confirmed status. Applies when the detection definition has no
    /// `[debounce]` table of its own.
    /// Defaults to 300 ms.
    pub confirm_ms: u64,

    /// Height of the window a `region = "bottom"` rule is matched against:
    /// this many lines counted upward from the last line holding a visible
    /// (non-whitespace) character. Applies when the detection definition
    /// omits `bottom_line_count`.
    /// Defaults to 5 lines.
    pub bottom_line_count: u16,

    /// Directory layer the detection definitions are loaded from, once
    /// resolved (CLI argument, environment, or the config root's `detection`
    /// directory). `None` means there is no directory layer.
    pub detection_dir: Option<PathBuf>,

    /// Name of the detection definition to load
    /// (`<detection_dir>/<name>.toml`).
    pub detection_name: String,

    /// Whether the definitions bundled into the binary are a layer for this
    /// daemon.
    ///
    /// Defaults to `false` so that a fixture directory answers alone: a name
    /// the fixture does not carry must not quietly resolve to a bundled
    /// definition. Production enables it from the resolved layers.
    pub bundled_definitions: bool,

    /// Upper bound driving the window registry's dual resource limits: at
    /// most this many *living* (non-`Exited`) windows, and at most
    /// `max_windows * 2` total entries (including `Exited` ones kept for
    /// their screen). Overridable via the `YAGURA_MAX_WINDOWS`
    /// environment variable (see [`resolve_max_windows`]).
    /// Defaults to 32.
    pub max_windows: usize,

    /// How `interrupt_window` delivers the interrupt. Defaults to
    /// [`InterruptMethod::Bytes`]. Overridable through a playbook's
    /// `[limits] interrupt_method`.
    pub interrupt_method: InterruptMethod,

    /// Byte sequence `interrupt_window` writes to a window's PTY under
    /// [`InterruptMethod::Bytes`].
    /// Defaults to `[0x03]` (Ctrl-C), which ConPTY turns into the console
    /// interrupt event. A CLI that does not react to it can be served by
    /// overriding this through a playbook's `[limits]`.
    pub interrupt_bytes: Vec<u8>,

    /// How long a graceful `close_window` waits for the child's exit
    /// notification before the Job Object is torn down.
    /// Defaults to 500 ms.
    pub close_grace_ms: u64,

    /// Upper bound `handle_close_window` waits, after the process tree is
    /// already torn down, for the window's *confirmed* status
    /// ([`crate::window::WindowEntry::current_status`]) to settle to `Exited`
    /// before replying `WindowClosed`. Overridable via the
    /// `YAGURA_CLOSE_SETTLE_MS` environment variable (see
    /// [`resolve_close_settle_ms`]).
    ///
    /// The GUI drops a closed window's pane without re-querying
    /// `list_windows`, while its periodic poll reopens a pane for any window
    /// it still sees as something other than `Exited` (`planPanes` in
    /// `gui/src/session/pane-plan.ts`). Waiting for the confirmed status --
    /// produced by a chain of tasks that can lag the reply -- makes that
    /// revival unlikely. It is a best-effort mitigation: reaching the bound
    /// never turns the reply into an error (fail-safe), and a window stuck in
    /// `Unknown` waits out the full bound.
    ///
    /// Defaults to 200 ms, which covers the observed settle latency (a
    /// sub-millisecond typical case, single-digit milliseconds at worst) with
    /// a wide margin while keeping the worst-case added reply latency
    /// (`close_grace_ms` + `close_settle_ms`) under a second.
    pub close_settle_ms: u64,

    /// How long after an interactive, non-submitting PTY write output is
    /// attributed to that input's echo and therefore not counted as
    /// activity evidence. Defaults to 150 ms: measured real-PTY echo round
    /// trips stay in single-digit milliseconds, so this covers their tail
    /// with a wide margin while staying well below the 300 ms a raw
    /// judgement must hold for by default.
    pub input_echo_window_ms: u64,

    /// How long after a PTY resize was requested its own redrawn output is
    /// attributed to that resize and therefore not counted as activity
    /// evidence. Defaults to 150 ms: measured ConPTY resize-to-redraw
    /// latency is sub-millisecond, rounded up to the same safe margin
    /// `input_echo_window_ms` uses rather than to the raw value.
    pub resize_redraw_window_ms: u64,

    /// Upper bound accepted for a `wait_for_status` timeout, in seconds.
    /// Larger requests are rejected fail-loud rather than clamped. Defaults
    /// to 600 s.
    pub wait_timeout_max_secs: u64,

    /// How many `wait_for_status` waiters a single connection may have in
    /// flight at once. Defaults to 16.
    pub max_waiters_per_conn: usize,

    /// Capacity of the daemon-wide `StatusEvent` token bucket, i.e. the
    /// burst it absorbs. Defaults to 10.
    pub status_event_burst: u32,

    /// Sustained `StatusEvent` rate the token bucket refills at, per second.
    /// Defaults to 5.
    pub status_event_per_sec: u32,

    /// The most bytes of a window's text `read_output` returns,
    /// bounding the work done per call. Defaults
    /// to 256 KiB.
    pub read_output_max_bytes: usize,

    /// Largest number of lines `read_output` may be asked for. Defaults to
    /// 1000.
    pub read_output_max_lines: usize,

    /// Playbook path resolved from `--playbook` / `YAGURA_PLAYBOOK`, kept for
    /// diagnostics: together with [`DaemonConfig::approval_policy`] it tells
    /// "no playbook configured" apart from "the playbook failed to load".
    pub playbook: Option<PathBuf>,

    /// The loaded playbook, or `None` when none was configured or it failed
    /// to load. `None` denies every destructive operation (fail-closed).
    ///
    /// This is the startup value only: [`crate::live_policy::PolicyCell`] is
    /// seeded from it once at boot, and every decision made afterwards reads
    /// the cell instead.
    pub approval_policy: Option<ApprovalPolicy>,

    /// Deadline for a pending approval request, in seconds. Defaults to
    /// [`APPROVAL_TIMEOUT_DEFAULT_SECS`]. Overridable through a playbook's
    /// `[limits] approval_timeout_secs`.
    pub approval_timeout_secs: u64,

    /// Deadline for a window opened by a handoff to become ready for input,
    /// in seconds. Defaults to [`HANDOFF_READY_TIMEOUT_DEFAULT_SECS`].
    /// Overridable through a playbook's `[limits] handoff_ready_timeout_secs`.
    pub handoff_ready_timeout_secs: u64,

    /// Daemon-wide cap on handoffs the daemon has accepted and not yet
    /// answered. Defaults to 32.
    pub max_open_handoffs: usize,

    /// Cap on handoffs a single connection has outstanding: accepted and not
    /// yet answered. Defaults to 4.
    pub max_open_handoffs_per_conn: usize,

    /// Daemon-wide cap on pending approval requests. Defaults to 32.
    pub max_pending_approvals: usize,

    /// Cap on pending approval requests held by a single requester
    /// connection. Defaults to 4.
    pub max_pending_approvals_per_conn: usize,

    /// Canonicalized path of the only executable whose connections may
    /// register as approvers, resolved from `--approver-exe`.
    ///
    /// Defaults to `None`, which is fail-closed: no connection can become an
    /// approver, so every destructive operation the playbook does not
    /// auto-approve ends as `approval_required`.
    pub approver_exe: Option<PathBuf>,

    /// Fixed status token to adopt instead of generating one, as a hex string
    /// of 32 characters. This exists so tests and development setups can
    /// present a known-valid token; production leaves it `None`.
    ///
    /// When it is set, *every* window adopts this one value, so the
    /// per-window token isolation described on
    /// [`crate::window::WindowEntry::verify_status_token`] does not apply.
    pub status_token_override: Option<RedactedString>,

    /// Explicit override for the session store's database path, the highest
    /// precedence tier of `store::resolve_db_path`.
    ///
    /// Reserved: no CLI entry point sets this today. It exists so a future
    /// flag or playbook override can win over `YAGURA_STORE_DB` and the
    /// default `yagura.db` in the instance directory without a `DaemonConfig`
    /// shape change. Defaults to `None`.
    pub store_db_path: Option<PathBuf>,

    /// The store database path actually in use, i.e. what
    /// `store::resolve_db_path` returned.
    ///
    /// [`DaemonConfig::store_db_path`] is an *input* to that resolution,
    /// while this is its *outcome*, including a path supplied by the
    /// environment or the `<LOCALAPPDATA>` default. `None` means persistence
    /// is disabled, which the restore-candidate read answers with an empty
    /// list rather than an error.
    ///
    /// The IPC layer reads the store through this field rather than through
    /// the registry's store-writer channel, which only carries the write
    /// direction. `main.rs` assigns it immediately after resolving the path
    /// and before wiring the store, so both directions name the same file.
    /// Defaults to `None`.
    pub store_db_path_resolved: Option<PathBuf>,

    /// Directory launch profiles are read from, resolved by
    /// `cli::resolve_profiles_dir`; it need not exist yet. With
    /// [`DaemonConfig::bundled_profiles`] a file there carries only the user's
    /// changes to the shipped profile of the same id.
    pub profiles_dir: Option<PathBuf>,

    /// Whether the profiles shipped with this build are the base layer the
    /// files in [`DaemonConfig::profiles_dir`] are laid over.
    ///
    /// Defaults to `false` so that a fixture directory answers alone.
    /// Production enables it unless the directory was given explicitly.
    pub bundled_profiles: bool,

    /// Path to the role -> assignment table (`roles.toml`), resolved by
    /// `cli::resolve_roles_path`. `None` means no config root could be
    /// composed (no `--roles`, no `YAGURA_ROLES`, no config base), so there
    /// is nowhere to read or save the table.
    pub roles_path: Option<PathBuf>,

    /// The operating system this daemon is running on, decided in exactly
    /// one place ([`ProfileOs::host`]) so that no other module re-derives
    /// it and risks disagreeing with this field. Selects which of a launch
    /// profile's operating-system-specific argv keys applies.
    pub host_os: ProfileOs,

    /// The executable-resolution environment (`PATH`, extra bin
    /// directories, the Windows command interpreter), read from the process
    /// environment exactly once at daemon start-up (`main.rs`) and
    /// held here for every launch-profile and `OpenWindow` resolution
    /// thereafter. A test injects a lookup of its own instead of reading the
    /// real environment.
    pub program_lookup: ProgramLookup,

    /// The predicate deciding whether a resolved candidate path can be
    /// started, shared by every executable-resolution call
    /// ([`crate::profile::unavailable_reason`], the registry's spawn path).
    /// A function pointer rather than a `Box<dyn Fn>` so this type keeps
    /// deriving `Clone` without paying for a second allocation on every
    /// clone of [`DaemonConfig`]. Defaults to
    /// [`yagura_os::program::default_is_executable`].
    pub program_is_executable: fn(&Path) -> bool,

    /// The default shell, resolved once by `main.rs` (via
    /// [`crate::resolve_shell`], the same input the initial window uses)
    /// and held here rather than re-read from the environment by every
    /// launch. `None` means resolution failed at start-up: a profile that
    /// omits `[command]` cannot launch until the daemon is restarted with a
    /// resolvable shell.
    pub default_shell: Option<PathBuf>,

    /// The environment laid over the daemon's own for the children it
    /// starts, resolved once by `main.rs` at
    /// start-up (via [`crate::child_env::compose_child_base_env`]). A window's
    /// child gets it first; a launching terminal's own variables are then
    /// removed from it (see [`crate::terminal_identity`]), and
    /// [`crate::terminal_identity::DECLARED_TERMINAL`] and `extra_env` win over
    /// a same-named entry. Its `Debug` output never shows a name or a value.
    pub child_base_env: ChildBaseEnv,

    /// The working directory a window opens in when neither the caller nor a
    /// profile names one, resolved once by `main.rs` at start-up (via
    /// [`resolve_default_window_cwd`]). `None` (the default) leaves such a
    /// window in the daemon's own current directory.
    pub default_window_cwd: Option<PathBuf>,

    /// The `[chat]` playbook section's numeric limits, merged in by
    /// [`crate::control::apply_chat_section`]. Defaults to
    /// [`room::ChatLimits::default`] (documented in the bundled `playbook.toml`).
    pub chat_limits: room::ChatLimits,

    /// The intro text painted once into a participant window at startup,
    /// into that window's own workspace room. Empty disables intro delivery.
    /// Defaults to a built-in text explaining how to use the room.
    pub chat_intro: String,

    /// Detection definition names excluded from chat participation.
    /// Defaults to `["generic-shell"]`.
    pub chat_exclude_detections: Vec<String>,

    /// Upper bound on how many chat deliveries (posts and intros together)
    /// may be in flight at once. Defaults to 32.
    pub chat_delivery_max: usize,

    /// Upper bound on how many deliveries (chat posts, intros and handoffs)
    /// may wait for one window's turn at a time. The delivery holding the
    /// turn is not counted; a delivery past the bound is turned away for that
    /// window alone. Defaults to 8.
    pub max_turn_waiters_per_window: usize,

    /// The MCP bridge executable (`yagura-mcp`) a window's CLI is pointed at,
    /// resolved once by `main.rs` next to the daemon's own executable and
    /// already in the form a child process can be handed. `None` when it
    /// could not be resolved: no window gets an MCP injection then.
    /// Defaults to `None`.
    pub mcp_bridge: Option<PathBuf>,

    /// Directory the MCP settings files a profile's `[mcp]` table asks for
    /// are written to. `None` means there is nowhere to write one, so a
    /// profile that needs a settings file gets no MCP injection. Defaults to
    /// `None`.
    pub mcp_config_dir: Option<PathBuf>,

    /// Directory holding the daemon's own executable under a fixed name
    /// (see [`prepare_report_dir`]), put in front of every child's `PATH` so
    /// a CLI's hook can name the daemon the same way from any shell. `None`
    /// when it could not be prepared: children's `PATH` is then left as is.
    /// Defaults to `None`.
    pub report_dir: Option<PathBuf>,

    /// Whether a newly created plan waits for the operator's approval before
    /// any of its task windows can open, merged in by
    /// [`crate::control::apply_plans_section`]. Defaults to `true`, so a
    /// missing or unreadable playbook never skips the operator's review.
    ///
    /// This is the startup value only: [`crate::live_policy::PolicyCell`] is
    /// seeded from it once at boot, and every decision made afterwards reads
    /// the cell instead.
    pub plan_approval_required: bool,

    /// How many `list_model_choices` listing commands may run at once across
    /// the whole daemon. A request beyond this is refused at once
    /// (`resource_exhausted`) rather than queued. Defaults to 2.
    pub model_choices_max_concurrent: usize,

    /// How long a `list_model_choices` listing command may run before its
    /// whole process tree is torn down. Defaults to 20 seconds.
    pub model_choices_timeout: Duration,

    /// The largest standard output a `list_model_choices` listing command
    /// may write before its whole process tree is torn down. Defaults to 4
    /// MiB.
    pub model_choices_max_output_bytes: usize,

    /// The identifier of the instance this daemon serves, `None` for the
    /// default instance. Threaded into a `[mcp_registration]` row's
    /// registration name and environment
    /// (`mcp_registration::RegistrationTarget::for_instance`), so a
    /// development instance never overwrites the default instance's own
    /// registration. Resolved once by `main.rs` from
    /// `yagura_os::InstanceNamespace::id`. Defaults to `None`.
    pub instance_id: Option<String>,

    /// The home directory a `[mcp_registration]` child is started in, and the
    /// base a host-form `status.file` is resolved against. `None` means no
    /// usable home was found: a host settings-file read then reports
    /// [`crate::mcp_registration::StatusObservation::Exited`] with no exit
    /// code rather than falling back to the daemon's own current directory.
    /// Resolved once by `main.rs` from `std::env::home_dir`. Defaults to
    /// `None`.
    pub home_dir: Option<PathBuf>,

    /// How long one `[mcp_registration]` command (a probe, a status read, or
    /// a registration or removal) may run before its whole process tree is
    /// torn down. Longer than `model_choices_timeout`: it has to cover a WSL
    /// distribution's first start and a CLI's own connection check while
    /// reading its status. Defaults to 60 seconds.
    pub mcp_registration_timeout: Duration,

    /// The largest standard output one `[mcp_registration]` command may write
    /// before its whole process tree is torn down. Defaults to 1 MiB.
    pub mcp_registration_max_output_bytes: usize,

    /// How many `[mcp_registration]` row state reads may run at once, within
    /// a single listing. Defaults to 2.
    pub mcp_registration_status_max_concurrent: usize,
}

/// A configuration value whose contents must never reach a `Debug` rendering.
///
/// [`DaemonConfig`] derives `Debug`, so a secret held as a plain `String`
/// would be printed verbatim by every `{:?}` of the configuration. Wrapping it
/// makes that impossible by construction: `Debug` prints a fixed mask, and the
/// raw characters are only reachable through the `Deref` to `str`, which no
/// logging path uses.
#[derive(Clone, PartialEq, Eq)]
pub struct RedactedString(String);

impl From<String> for RedactedString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for RedactedString {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl std::ops::Deref for RedactedString {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for RedactedString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("\"***\"")
    }
}

/// Default value of [`DaemonConfig::max_windows`] when nothing overrides it.
const DEFAULT_MAX_WINDOWS: usize = 32;

/// Default value of [`DaemonConfig::close_settle_ms`] when nothing overrides
/// it. See that field's doc comment for the measurement this margin is
/// derived from.
const DEFAULT_CLOSE_SETTLE_MS: u64 = 200;

/// Default value of [`DaemonConfig::input_echo_window_ms`].
const DEFAULT_INPUT_ECHO_WINDOW_MS: u64 = 150;

/// Default value of [`DaemonConfig::resize_redraw_window_ms`]. See that
/// field's doc comment for the measurement this margin is derived from.
const DEFAULT_RESIZE_REDRAW_WINDOW_MS: u64 = 150;

/// Ctrl-C, the default [`DaemonConfig::interrupt_bytes`] payload.
const CTRL_C: u8 = 0x03;

/// Default value of [`DaemonConfig::approval_timeout_secs`] when nothing
/// overrides it: the single definition point every default (this one and
/// [`DaemonConfig::default`]) and every range check derives from.
pub const APPROVAL_TIMEOUT_DEFAULT_SECS: u64 = 60;

/// Default value of [`DaemonConfig::handoff_ready_timeout_secs`] when nothing
/// overrides it: the single definition point every default (this one and
/// [`DaemonConfig::default`]) derives from.
pub const HANDOFF_READY_TIMEOUT_DEFAULT_SECS: u64 = 60;

/// Default value of [`DaemonConfig::max_open_handoffs`].
const DEFAULT_MAX_OPEN_HANDOFFS: usize = 32;

/// Default value of [`DaemonConfig::max_open_handoffs_per_conn`].
const DEFAULT_MAX_OPEN_HANDOFFS_PER_CONN: usize = 4;

/// Default value of [`DaemonConfig::max_pending_approvals`].
const DEFAULT_MAX_PENDING_APPROVALS: usize = 32;

/// Default value of [`DaemonConfig::max_pending_approvals_per_conn`].
const DEFAULT_MAX_PENDING_APPROVALS_PER_CONN: usize = 4;

/// Default value of [`DaemonConfig::chat_intro`].
const DEFAULT_CHAT_INTRO: &str = "A chat room for this workspace is available. Use the \
     read_chat and send_chat MCP tools to talk with the other windows opened in the same git \
     repository (or directory) and the user; pass room_id \"main\" to reach every window, reply \
     with the room_id from a message's reply field, and address a message by mentioning another \
     window's @name, @parent for the window that opened you or owns your plan, or @user to ask \
     for a ruling.";

/// Default value of [`DaemonConfig::chat_delivery_max`].
const DEFAULT_CHAT_DELIVERY_MAX: usize = 32;

/// Default value of [`DaemonConfig::max_turn_waiters_per_window`].
const DEFAULT_MAX_TURN_WAITERS_PER_WINDOW: usize = 8;

/// Default value of [`DaemonConfig::model_choices_max_concurrent`].
const DEFAULT_MODEL_CHOICES_MAX_CONCURRENT: usize = 2;

/// Default value of [`DaemonConfig::model_choices_timeout`], in seconds.
const DEFAULT_MODEL_CHOICES_TIMEOUT_SECS: u64 = 20;

/// Default value of [`DaemonConfig::model_choices_max_output_bytes`]: 4 MiB.
const DEFAULT_MODEL_CHOICES_MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// Default value of [`DaemonConfig::mcp_registration_timeout`], in seconds.
const DEFAULT_MCP_REGISTRATION_TIMEOUT_SECS: u64 = 60;

/// Default value of [`DaemonConfig::mcp_registration_max_output_bytes`]: 1
/// MiB.
const DEFAULT_MCP_REGISTRATION_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// Default value of [`DaemonConfig::mcp_registration_status_max_concurrent`].
const DEFAULT_MCP_REGISTRATION_STATUS_MAX_CONCURRENT: usize = 2;

/// Every `DaemonConfig` value the daemon binary resolves at startup.
///
/// It has no `Default` and [`DaemonConfig::from_startup`] destructures it
/// without `..`, so a value the binary resolves but forgets to hand over, or a
/// field added here but not carried into the config, stops the build instead
/// of silently falling back to the default.
#[derive(Debug)]
pub struct StartupParts {
    /// Becomes [`DaemonConfig::detection_dir`].
    pub detection_dir: Option<PathBuf>,
    /// Becomes [`DaemonConfig::bundled_definitions`].
    pub bundled_definitions: bool,
    /// `--detection`; `None` keeps the default name.
    pub detection_name: Option<String>,
    /// Becomes [`DaemonConfig::max_windows`].
    pub max_windows: usize,
    /// Becomes [`DaemonConfig::close_settle_ms`].
    pub close_settle_ms: u64,
    /// Becomes [`DaemonConfig::input_echo_window_ms`].
    pub input_echo_window_ms: u64,
    /// Becomes [`DaemonConfig::resize_redraw_window_ms`].
    pub resize_redraw_window_ms: u64,
    /// Becomes [`DaemonConfig::playbook`].
    pub playbook: Option<PathBuf>,
    /// Becomes [`DaemonConfig::approval_policy`].
    pub approval_policy: Option<ApprovalPolicy>,
    /// Becomes [`DaemonConfig::approver_exe`].
    pub approver_exe: Option<PathBuf>,
    /// Becomes [`DaemonConfig::status_token_override`].
    pub status_token_override: Option<RedactedString>,
    /// Becomes [`DaemonConfig::profiles_dir`].
    pub profiles_dir: Option<PathBuf>,
    /// Becomes [`DaemonConfig::bundled_profiles`].
    pub bundled_profiles: bool,
    /// Becomes [`DaemonConfig::roles_path`].
    pub roles_path: Option<PathBuf>,
    /// Becomes [`DaemonConfig::program_lookup`].
    pub program_lookup: ProgramLookup,
    /// Becomes [`DaemonConfig::child_base_env`].
    pub child_base_env: ChildBaseEnv,
    /// Becomes [`DaemonConfig::default_window_cwd`].
    pub default_window_cwd: Option<PathBuf>,
}

impl DaemonConfig {
    /// Builds the configuration the daemon binary runs with: every startup
    /// value from `parts`, every other field at its default.
    #[must_use]
    pub fn from_startup(parts: StartupParts) -> Self {
        let StartupParts {
            detection_dir,
            bundled_definitions,
            detection_name,
            max_windows,
            close_settle_ms,
            input_echo_window_ms,
            resize_redraw_window_ms,
            playbook,
            approval_policy,
            approver_exe,
            status_token_override,
            profiles_dir,
            bundled_profiles,
            roles_path,
            program_lookup,
            child_base_env,
            default_window_cwd,
        } = parts;
        let defaults = Self::default();
        Self {
            detection_dir,
            bundled_definitions,
            detection_name: detection_name.unwrap_or(defaults.detection_name),
            max_windows,
            close_settle_ms,
            input_echo_window_ms,
            resize_redraw_window_ms,
            playbook,
            approval_policy,
            approver_exe,
            status_token_override,
            profiles_dir,
            bundled_profiles,
            roles_path,
            program_lookup,
            child_base_env,
            default_window_cwd,
            ..Self::default()
        }
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            confirm_ms: 300,
            bottom_line_count: 5,
            detection_dir: None,
            detection_name: DEFAULT_DETECTION_NAME.to_string(),
            bundled_definitions: false,
            max_windows: DEFAULT_MAX_WINDOWS,
            interrupt_method: InterruptMethod::Bytes,
            interrupt_bytes: vec![CTRL_C],
            close_grace_ms: 500,
            close_settle_ms: DEFAULT_CLOSE_SETTLE_MS,
            input_echo_window_ms: DEFAULT_INPUT_ECHO_WINDOW_MS,
            resize_redraw_window_ms: DEFAULT_RESIZE_REDRAW_WINDOW_MS,
            wait_timeout_max_secs: 600,
            max_waiters_per_conn: 16,
            status_event_burst: 10,
            status_event_per_sec: 5,
            read_output_max_bytes: 256 * 1024,
            read_output_max_lines: 1000,
            playbook: None,
            approval_policy: None,
            approval_timeout_secs: APPROVAL_TIMEOUT_DEFAULT_SECS,
            handoff_ready_timeout_secs: HANDOFF_READY_TIMEOUT_DEFAULT_SECS,
            max_open_handoffs: DEFAULT_MAX_OPEN_HANDOFFS,
            max_open_handoffs_per_conn: DEFAULT_MAX_OPEN_HANDOFFS_PER_CONN,
            max_pending_approvals: DEFAULT_MAX_PENDING_APPROVALS,
            max_pending_approvals_per_conn: DEFAULT_MAX_PENDING_APPROVALS_PER_CONN,
            approver_exe: None,
            status_token_override: None,
            store_db_path: None,
            store_db_path_resolved: None,
            profiles_dir: None,
            bundled_profiles: false,
            roles_path: None,
            host_os: ProfileOs::host(),
            program_lookup: ProgramLookup {
                path_dirs: Vec::new(),
                extra_bin_dirs: Vec::new(),
                windows_comspec: None,
            },
            program_is_executable: yagura_os::program::default_is_executable,
            default_shell: None,
            child_base_env: ChildBaseEnv::default(),
            default_window_cwd: None,
            chat_limits: room::ChatLimits::default(),
            chat_intro: DEFAULT_CHAT_INTRO.to_string(),
            chat_exclude_detections: vec![DEFAULT_DETECTION_NAME.to_string()],
            chat_delivery_max: DEFAULT_CHAT_DELIVERY_MAX,
            max_turn_waiters_per_window: DEFAULT_MAX_TURN_WAITERS_PER_WINDOW,
            mcp_bridge: None,
            mcp_config_dir: None,
            report_dir: None,
            plan_approval_required: crate::control::PLAN_APPROVAL_REQUIRED_BY_DEFAULT,
            model_choices_max_concurrent: DEFAULT_MODEL_CHOICES_MAX_CONCURRENT,
            model_choices_timeout: Duration::from_secs(DEFAULT_MODEL_CHOICES_TIMEOUT_SECS),
            model_choices_max_output_bytes: DEFAULT_MODEL_CHOICES_MAX_OUTPUT_BYTES,
            instance_id: None,
            home_dir: None,
            mcp_registration_timeout: Duration::from_secs(DEFAULT_MCP_REGISTRATION_TIMEOUT_SECS),
            mcp_registration_max_output_bytes: DEFAULT_MCP_REGISTRATION_MAX_OUTPUT_BYTES,
            mcp_registration_status_max_concurrent: DEFAULT_MCP_REGISTRATION_STATUS_MAX_CONCURRENT,
        }
    }
}

/// Composes the daemon's log directory: `<state_base>/<namespace>/logs`.
///
/// A pure function, so `main.rs`'s `open_log_file` (the sole caller) can be
/// exercised from a test without touching the filesystem itself.
#[must_use]
pub fn log_dir(state_base: &Path, namespace: &InstanceNamespace) -> PathBuf {
    yagura_os::instance_dir(state_base, namespace).join("logs")
}

/// Composes the daemon's report directory: `<state_base>/<namespace>/bin`.
///
/// Not the installation folder: that one also holds files (an uninstaller)
/// a shell in a window must not resolve by name.
#[must_use]
pub fn report_dir(state_base: &Path, namespace: &InstanceNamespace) -> PathBuf {
    yagura_os::instance_dir(state_base, namespace).join("bin")
}

/// The fixed name a CLI's hook runs the daemon by.
pub const REPORT_EXE_NAME: &str = if cfg!(windows) {
    "yagura-daemon.exe"
} else {
    "yagura-daemon"
};

/// Blocking: places `exe` in `dir` (below `state_base`) as
/// [`REPORT_EXE_NAME`], replacing whatever an earlier start left there.
///
/// A hard link when the filesystem allows one, otherwise a copy; either is
/// staged under a temporary name and renamed over the target, so a hook never
/// runs a half-written file. On Unix `dir` is narrowed to owner-only like the
/// rest of the state directory.
///
/// # Errors
///
/// A description of the step that failed: `dir` could not be created (for
/// example a file is in its place), `exe` could be neither linked nor
/// copied, or the rename failed while the target still differs from `exe`.
pub fn prepare_report_dir(state_base: &Path, dir: &Path, exe: &Path) -> Result<(), String> {
    yagura_os::secure_owned_dir(state_base, dir)?;
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("failed to create {}: {err}", dir.display()))?;
    let target = dir.join(REPORT_EXE_NAME);
    let temp_prefix = format!(".{REPORT_EXE_NAME}.tmp-");
    remove_stale_temps(dir, &temp_prefix);
    let temp = dir.join(format!("{temp_prefix}{}", std::process::id()));

    if std::fs::hard_link(exe, &temp).is_err() {
        std::fs::copy(exe, &temp).map_err(|err| {
            let _ = std::fs::remove_file(&temp);
            format!(
                "failed to place {} in {}: {err}",
                exe.display(),
                dir.display()
            )
        })?;
    }
    match std::fs::rename(&temp, &target) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&temp);
            // Windows refuses to replace a file whose image is running, which is
            // the case when the target is already a link to this very executable.
            if same_content(&target, exe) {
                Ok(())
            } else {
                Err(format!("failed to replace {}: {err}", target.display()))
            }
        }
    }
}

/// Removes staging files an interrupted earlier start left in `dir`, so the
/// directory ends up holding the executable alone.
fn remove_stale_temps(dir: &Path, prefix: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(prefix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn same_content(a: &Path, b: &Path) -> bool {
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Parses the `YAGURA_MAX_WINDOWS` environment variable value.
///
/// A missing value, one that fails to parse as a `usize`, or `0` (which
/// would make every `open_window` call fail) all fall back to `default`
/// (fail-safe: a malformed override must never take the daemon down or
/// silently disable the limit). Pulled out as a pure function so this
/// parsing is unit-testable without spawning the daemon.
pub fn resolve_max_windows(raw: Option<&str>, default: usize) -> usize {
    raw.and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|&value| value > 0)
        .unwrap_or(default)
}

/// Shared fail-safe parse for the `u64` env-var resolvers below: a missing
/// value, one that fails to parse as a `u64`, or `0` all fall back to
/// `default`.
fn resolve_env_u64(raw: Option<&str>, default: u64) -> u64 {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|&value| value > 0)
        .unwrap_or(default)
}

/// Parses the `YAGURA_CLOSE_SETTLE_MS` environment variable value.
///
/// Same fail-safe shape as [`resolve_max_windows`]: a missing value, one
/// that fails to parse as a `u64`, or `0` (which would make the settle wait
/// a no-op and reopen the stale-read race [`DaemonConfig::close_settle_ms`]
/// exists to close) all fall back to `default`.
pub fn resolve_close_settle_ms(raw: Option<&str>, default: u64) -> u64 {
    resolve_env_u64(raw, default)
}

/// Parses the `YAGURA_INPUT_ECHO_WINDOW_MS` environment variable value.
///
/// Same fail-safe shape as [`resolve_close_settle_ms`]: a missing value, one
/// that fails to parse as a `u64`, or `0` all fall back to `default`.
pub fn resolve_input_echo_window_ms(raw: Option<&str>, default: u64) -> u64 {
    resolve_env_u64(raw, default)
}

/// Parses the `YAGURA_RESIZE_REDRAW_WINDOW_MS` environment variable value.
///
/// Same fail-safe shape as [`resolve_input_echo_window_ms`]: a missing
/// value, one that fails to parse as a `u64`, or `0` all fall back to
/// `default`. A valid override is never clamped: the config layer does not
/// know a definition's `window_ms`, so it cannot judge an upper bound.
pub fn resolve_resize_redraw_window_ms(raw: Option<&str>, default: u64) -> u64 {
    resolve_env_u64(raw, default)
}

/// Resolves the `YAGURA_STATUS_TOKEN_OVERRIDE` environment variable value
/// into [`DaemonConfig::status_token_override`].
///
/// A missing or blank value resolves to `None`, the expected production
/// setting: the override bypasses the OS RNG, so its secrecy is only as
/// strong as the environment variable's own visibility. Well-formedness
/// (32-character hex) is validated downstream by
/// `StatusToken::from_override`, mirroring how [`resolve_playbook`] resolves
/// a path and leaves loading to its caller.
///
/// [`resolve_playbook`]: crate::cli::resolve_playbook
pub fn resolve_status_token_override(raw: Option<&str>) -> Option<RedactedString> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(RedactedString::from)
}

/// Whether the daemon's own inherited `TERM` shows it was started from a
/// terminal: a missing or empty value means it was not (Finder, Dock, a
/// desktop launcher). [`crate::child_env::login_env_decision`], and through
/// it [`resolve_default_window_cwd`], rest on it.
#[must_use]
pub fn inherits_terminal(term: Option<&OsStr>) -> bool {
    term.is_some_and(|value| !value.is_empty())
}

/// Resolves what [`DaemonConfig::default_window_cwd`] should hold.
///
/// A daemon started without a terminal (the case in which
/// [`crate::child_env::login_env_decision`] reads the login shell) inherits
/// whatever directory its launcher left it in, often the filesystem root,
/// whereas a terminal opens its shell in the home directory. So in that case
/// only, the home directory stands in: `HOME` from `base_env`, else
/// `inherited_home`, provided `exists_dir` says it is a directory. Every
/// other case returns `None`, keeping the daemon's own current directory.
#[must_use]
pub fn resolve_default_window_cwd(
    host_os: ProfileOs,
    term: Option<&OsStr>,
    base_env: &ChildBaseEnv,
    inherited_home: Option<&OsStr>,
    exists_dir: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if crate::child_env::login_env_decision(host_os, term)
        != crate::child_env::LoginEnvDecision::Read
    {
        return None;
    }
    base_env
        .get(OsStr::new("HOME"))
        .or(inherited_home)
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .filter(|home| exists_dir(home))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use yagura_os::program::ProgramLookup;

    use super::{DaemonConfig, StartupParts, resolve_status_token_override};
    use crate::child_env::ChildBaseEnv;
    use crate::control::ApprovalPolicy;

    #[test]
    fn a_status_token_override_is_kept_trimmed_and_a_blank_one_is_no_override() {
        assert_eq!(
            resolve_status_token_override(Some(" 0123456789abcdef0123456789abcdef \n")).as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(resolve_status_token_override(Some(" \t")), None);
        assert_eq!(resolve_status_token_override(None), None);
    }

    #[test]
    fn the_capture_output_limits_default_to_their_documented_sizes() {
        let config = DaemonConfig::default();
        assert_eq!(config.model_choices_max_output_bytes, 4 << 20);
        assert_eq!(config.mcp_registration_max_output_bytes, 1 << 20);
    }

    fn bare_parts() -> StartupParts {
        StartupParts {
            detection_dir: None,
            bundled_definitions: false,
            detection_name: None,
            max_windows: 1,
            close_settle_ms: 1,
            input_echo_window_ms: 1,
            resize_redraw_window_ms: 1,
            playbook: None,
            approval_policy: None,
            approver_exe: None,
            status_token_override: None,
            profiles_dir: None,
            bundled_profiles: false,
            roles_path: None,
            program_lookup: ProgramLookup {
                path_dirs: Vec::new(),
                extra_bin_dirs: Vec::new(),
                windows_comspec: None,
            },
            child_base_env: ChildBaseEnv::default(),
            default_window_cwd: None,
        }
    }

    #[test]
    fn from_startup_carries_every_startup_value_into_the_config() {
        let defaults = DaemonConfig::default();
        let path = |name: &str| Some(PathBuf::from(name));
        let config = DaemonConfig::from_startup(StartupParts {
            detection_dir: path("detection-dir"),
            bundled_definitions: !defaults.bundled_definitions,
            detection_name: Some("named-definition".to_string()),
            max_windows: defaults.max_windows + 1,
            close_settle_ms: defaults.close_settle_ms + 1,
            input_echo_window_ms: defaults.input_echo_window_ms + 1,
            resize_redraw_window_ms: defaults.resize_redraw_window_ms + 1,
            playbook: path("playbook.toml"),
            approval_policy: Some(ApprovalPolicy::default()),
            approver_exe: path("approver-exe"),
            status_token_override: Some("0123456789abcdef0123456789abcdef".into()),
            profiles_dir: path("profiles-dir"),
            bundled_profiles: !defaults.bundled_profiles,
            roles_path: path("roles.toml"),
            program_lookup: ProgramLookup {
                path_dirs: vec![PathBuf::from("lookup-dir")],
                extra_bin_dirs: Vec::new(),
                windows_comspec: None,
            },
            child_base_env: ChildBaseEnv::from_pairs(vec![(
                OsString::from("STARTUP_PROBE"),
                OsString::from("1"),
            )]),
            default_window_cwd: path("window-cwd"),
        });

        assert_eq!(config.detection_dir, path("detection-dir"));
        assert_eq!(config.bundled_definitions, !defaults.bundled_definitions);
        assert_eq!(config.detection_name, "named-definition");
        assert_eq!(config.max_windows, defaults.max_windows + 1);
        assert_eq!(config.close_settle_ms, defaults.close_settle_ms + 1);
        assert_eq!(
            config.input_echo_window_ms,
            defaults.input_echo_window_ms + 1
        );
        assert_eq!(
            config.resize_redraw_window_ms,
            defaults.resize_redraw_window_ms + 1
        );
        assert_eq!(config.playbook, path("playbook.toml"));
        assert!(config.approval_policy.is_some());
        assert_eq!(config.approver_exe, path("approver-exe"));
        assert_eq!(
            config.status_token_override.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(config.profiles_dir, path("profiles-dir"));
        assert_eq!(config.bundled_profiles, !defaults.bundled_profiles);
        assert_eq!(config.roles_path, path("roles.toml"));
        assert_eq!(
            config.program_lookup.path_dirs,
            vec![PathBuf::from("lookup-dir")]
        );
        assert_eq!(config.child_base_env.len(), 1);
        assert_eq!(config.default_window_cwd, path("window-cwd"));
    }

    #[test]
    fn from_startup_keeps_the_default_detection_name_when_none_is_given() {
        let config = DaemonConfig::from_startup(bare_parts());
        assert_eq!(
            config.detection_name,
            DaemonConfig::default().detection_name
        );
    }
}
