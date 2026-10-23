//! Command-line argument resolution for the `yagura-daemon` binary.
//!
//! Every argument is resolved here by pure functions, leaving `main.rs` with
//! dispatching on the result and filling in [`crate::DaemonConfig`]; the
//! binary's startup sequence cannot be driven from an integration test, but
//! these functions can.
//!
//! One default is *not* decided here: an omitted `--detection` stays `None`
//! so `DaemonConfig::default()` remains the single source of truth for the
//! default definition name.
//!
//! Flags are scoped to one command: `--detection` / `--detection-dir` /
//! `--playbook` / `--profiles-dir` / `--approver-exe` / `--roles` configure
//! the daemon itself and are ignored on the client-side paths -- `--emit-status`,
//! `--deliver-context`, `--deliver-take`, `--deliver-wait`, `--report-session` and
//! `--shutdown` -- none of which starts a detection engine or an approval gate,
//! just as `--source` qualifies `--emit-status` only.

use std::ffi::OsString;
use std::path::PathBuf;

use serde::Deserialize;
use serde::de::IntoDeserializer;
use thiserror::Error;
use yagura_os::{InstanceIdError, InstanceNamespace};
use yagura_protocol::{WindowStatus, is_valid_source};

use crate::detector::definition::is_valid_detection_name;

/// `--source` value used when `--emit-status` is invoked without one.
const DEFAULT_EMIT_STATUS_SOURCE: &str = "cli";

/// Name of the directory holding detection definitions under the config
/// root.
const DETECTION_DIR_NAME: &str = "detection";

/// What the command line asked the daemon binary to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    /// Run the daemon itself.
    Run {
        /// Detection definition selected with `--detection`. `None` means the
        /// flag was omitted and `DaemonConfig::default()` decides.
        detection_name: Option<String>,
        /// Definition directory given with `--detection-dir`, i.e. stage (1)
        /// of [`resolve_detection_dir`].
        detection_dir_arg: Option<PathBuf>,
        /// Playbook path given with `--playbook`, i.e. stage (1) of
        /// [`resolve_playbook`]. `None` leaves the approval gate denying
        /// every destructive operation (fail-closed).
        playbook_arg: Option<PathBuf>,
        /// Launch-profiles directory given with `--profiles-dir`, i.e.
        /// stage (1) of [`resolve_profiles_dir`].
        profiles_dir_arg: Option<PathBuf>,
        /// Approver executable given with `--approver-exe`, verbatim as
        /// typed. `None` leaves approver registration refused for every
        /// connection (fail-closed). Canonicalizing it is `main.rs`'s job,
        /// the same split of duties `--playbook` uses.
        approver_exe_arg: Option<PathBuf>,
        /// Role table path given with `--roles`, i.e. stage (1) of
        /// [`resolve_roles_path`].
        roles_arg: Option<PathBuf>,
    },
    /// Ask an already-running daemon to terminate (`--shutdown`).
    Shutdown,
    /// Send a single structured-source status event to a running daemon
    /// (`--emit-status`).
    EmitStatus {
        /// The reported status.
        status: WindowStatus,
        /// Reporting source name, defaulting to `"cli"`.
        source: String,
    },
    /// Print the start-up context of the window the hook runs in
    /// (`--deliver-context`).
    DeliverContext,
    /// Print the parcels waiting for the window the hook runs in
    /// (`--deliver-take`).
    DeliverTake,
    /// Wait for a parcel for the window the hook runs in (`--deliver-wait`).
    DeliverWait,
    /// Tell the daemon the session the CLI's hook input names
    /// (`--report-session`).
    ReportSession {
        /// The JSON pointer (RFC 6901) of the string in the hook's stdin.
        pointer: String,
    },
}

impl CliCommand {
    /// Whether a CLI's hook runs this command: it only acts inside a window
    /// and is silent anywhere else.
    #[must_use]
    pub fn is_hook_command(&self) -> bool {
        matches!(
            self,
            Self::EmitStatus { .. }
                | Self::DeliverContext
                | Self::DeliverTake
                | Self::DeliverWait
                | Self::ReportSession { .. }
        )
    }
}

/// Why a command line was rejected.
///
/// Every rejection is fail-loud: no value is silently dropped, clamped or
/// guessed at.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CliError {
    /// An argument that is not part of the daemon's command line.
    #[error("unknown argument: {0}")]
    UnknownArgument(String),
    /// A flag that takes a value was given as the last argument.
    #[error("missing value for {flag}")]
    MissingValue {
        /// The flag whose value is missing.
        flag: String,
    },
    /// A flag's value failed validation.
    #[error("invalid value for {flag}: {value:?}")]
    InvalidValue {
        /// The flag that carried the rejected value.
        flag: String,
        /// The rejected value.
        value: String,
    },
    /// Two mutually exclusive commands were requested at once.
    #[error(
        "only one of --shutdown, --emit-status, --deliver-context, --deliver-take, \
         --deliver-wait and --report-session may be given"
    )]
    ConflictingCommands,
}

/// Resolves `args` (the command line *without* the program name) into the
/// single command the binary should perform.
///
/// Values are validated at parse time against the same rules the receiving
/// side enforces -- `is_valid_detection_name` for `--detection`,
/// [`is_valid_source`] for `--source` -- so an invalid command line never reaches the
/// daemon. When a flag is repeated, the last occurrence wins.
///
/// # Errors
///
/// Returns [`CliError`] for an unknown argument, a flag missing its value, a
/// value that fails validation, or two of the client-side commands combined.
pub fn parse_args(args: &[String]) -> Result<CliCommand, CliError> {
    let mut shutdown = false;
    let mut emit_status: Option<WindowStatus> = None;
    let mut source: Option<String> = None;
    let mut hook_door: Option<CliCommand> = None;
    let mut detection_name: Option<String> = None;
    let mut detection_dir_arg: Option<PathBuf> = None;
    let mut playbook_arg: Option<PathBuf> = None;
    let mut profiles_dir_arg: Option<PathBuf> = None;
    let mut approver_exe_arg: Option<PathBuf> = None;
    let mut roles_arg: Option<PathBuf> = None;

    let mut remaining = args.iter();
    while let Some(arg) = remaining.next() {
        match arg.as_str() {
            "--shutdown" => shutdown = true,
            "--emit-status" => {
                let value = next_value(&mut remaining, "--emit-status")?;
                emit_status =
                    Some(parse_status(value).ok_or_else(|| invalid("--emit-status", value))?);
            }
            "--deliver-context" => set_hook_door(&mut hook_door, CliCommand::DeliverContext)?,
            "--deliver-take" => set_hook_door(&mut hook_door, CliCommand::DeliverTake)?,
            "--deliver-wait" => set_hook_door(&mut hook_door, CliCommand::DeliverWait)?,
            "--report-session" => {
                let value = next_value(&mut remaining, "--report-session")?;
                if !value.starts_with('/') {
                    return Err(invalid("--report-session", value));
                }
                set_hook_door(
                    &mut hook_door,
                    CliCommand::ReportSession {
                        pointer: value.clone(),
                    },
                )?;
            }
            "--source" => {
                let value = next_value(&mut remaining, "--source")?;
                if !is_valid_source(value) {
                    return Err(invalid("--source", value));
                }
                source = Some(value.clone());
            }
            "--detection" => {
                let value = next_value(&mut remaining, "--detection")?;
                if !is_valid_detection_name(value) {
                    return Err(invalid("--detection", value));
                }
                detection_name = Some(value.clone());
            }
            "--detection-dir" => {
                let value = next_value(&mut remaining, "--detection-dir")?;
                if value.is_empty() {
                    return Err(invalid("--detection-dir", value));
                }
                detection_dir_arg = Some(PathBuf::from(value));
            }
            "--playbook" => {
                let value = next_value(&mut remaining, "--playbook")?;
                if value.is_empty() {
                    return Err(invalid("--playbook", value));
                }
                playbook_arg = Some(PathBuf::from(value));
            }
            "--profiles-dir" => {
                let value = next_value(&mut remaining, "--profiles-dir")?;
                if value.is_empty() {
                    return Err(invalid("--profiles-dir", value));
                }
                profiles_dir_arg = Some(PathBuf::from(value));
            }
            "--approver-exe" => {
                let value = next_value(&mut remaining, "--approver-exe")?;
                if value.is_empty() {
                    return Err(invalid("--approver-exe", value));
                }
                approver_exe_arg = Some(PathBuf::from(value));
            }
            "--roles" => {
                let value = next_value(&mut remaining, "--roles")?;
                if value.is_empty() {
                    return Err(invalid("--roles", value));
                }
                roles_arg = Some(PathBuf::from(value));
            }
            other => return Err(CliError::UnknownArgument(other.to_string())),
        }
    }

    let requested = usize::from(shutdown)
        + usize::from(emit_status.is_some())
        + usize::from(hook_door.is_some());
    if requested > 1 {
        return Err(CliError::ConflictingCommands);
    }
    if let Some(door) = hook_door {
        return Ok(door);
    }
    match (shutdown, emit_status) {
        (true, _) => Ok(CliCommand::Shutdown),
        (false, Some(status)) => Ok(CliCommand::EmitStatus {
            status,
            source: source.unwrap_or_else(|| DEFAULT_EMIT_STATUS_SOURCE.to_string()),
        }),
        (false, None) => Ok(CliCommand::Run {
            detection_name,
            detection_dir_arg,
            playbook_arg,
            profiles_dir_arg,
            approver_exe_arg,
            roles_arg,
        }),
    }
}

/// Records `command` as the hook door asked for. Repeating one door keeps the
/// last occurrence; two different doors conflict.
fn set_hook_door(slot: &mut Option<CliCommand>, command: CliCommand) -> Result<(), CliError> {
    if let Some(existing) = slot
        && std::mem::discriminant(existing) != std::mem::discriminant(&command)
    {
        return Err(CliError::ConflictingCommands);
    }
    *slot = Some(command);
    Ok(())
}

/// Where the directory layer of the detection definitions came from.
///
/// The two directory-carrying variants differ in what they imply for the
/// bundled definitions: a directory the user pointed at answers alone, while
/// one merely found under the config root is overlaid on the bundled
/// definitions ([`crate::detector::layers::DetectionLayers`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectionDirResolution {
    /// Given by `--detection-dir` or a non-empty `YAGURA_DETECTION_DIR`.
    Explicit(PathBuf),
    /// The `detection` directory under the config root, adopted because it
    /// exists.
    ConfigRoot(PathBuf),
    /// No directory layer at all.
    Unresolved,
}

/// Resolves the directory detection definitions are loaded from, in this
/// order: (1) `--detection-dir`, (2) the `YAGURA_DETECTION_DIR` environment
/// variable (an empty value counts as unset), (3) the `detection` directory
/// under `namespace`'s config root (the same layout `seed_config_file` seeds
/// the default playbook into) -- adopted only when it already exists as a
/// directory, empty or not.
///
/// Stage (1)/(2) is never second-guessed: a directory that does not exist is
/// still what that stage returns, and whether it carries the requested
/// definition is decided at load time. There is no executable-adjacent or
/// ancestor search, so nothing outside the two places above can become the
/// directory layer.
///
/// [`DetectionDirResolution::Unresolved`] leaves the bundled definitions as
/// the only source, which keeps a daemon with no configuration of its own
/// detecting rather than pinned to `Unknown`.
///
/// The caller supplies all three path inputs, so this function reads the
/// environment itself nowhere and the filesystem only for stage (3)'s
/// existence check.
#[must_use]
pub fn resolve_detection_dir(
    arg: Option<PathBuf>,
    env: Option<OsString>,
    config_base: Option<PathBuf>,
    namespace: &InstanceNamespace,
) -> DetectionDirResolution {
    if let Some(arg) = arg {
        return DetectionDirResolution::Explicit(arg);
    }
    if let Some(env) = env.filter(|value| !value.is_empty()) {
        return DetectionDirResolution::Explicit(PathBuf::from(env));
    }
    let Some(base) = config_base else {
        return DetectionDirResolution::Unresolved;
    };
    let candidate = yagura_os::instance_dir(&base, namespace).join(DETECTION_DIR_NAME);
    if candidate.is_dir() {
        DetectionDirResolution::ConfigRoot(candidate)
    } else {
        DetectionDirResolution::Unresolved
    }
}

/// Resolves the playbook path: (1) `--playbook`, (2) the `YAGURA_PLAYBOOK`
/// environment variable.
///
/// `None` means no playbook was configured, which is what keeps the approval
/// gate denying every destructive operation (fail-closed). An empty
/// environment value counts as unset, so exporting `YAGURA_PLAYBOOK=` does
/// not turn into a load failure for the path `""`.
///
/// The caller supplies both inputs, so this function does not read the
/// environment itself.
#[must_use]
pub fn resolve_playbook(arg: Option<PathBuf>, env: Option<std::ffi::OsString>) -> Option<PathBuf> {
    arg.or_else(|| env.filter(|value| !value.is_empty()).map(PathBuf::from))
}

/// Where the launch-profile directory was found, which decides whether the
/// profiles shipped with this build are laid under it: a directory given
/// explicitly answers alone, the config root's is laid over the shipped ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfilesDirResolution {
    /// Given by `--profiles-dir` or a non-empty `YAGURA_PROFILES_DIR`.
    Explicit(PathBuf),
    /// The `profiles` directory under the config root, whether or not it
    /// exists yet: a user's first change creates it.
    ConfigRoot(PathBuf),
    /// No config root could be resolved: the shipped profiles alone.
    Unresolved,
}

/// Resolves the launch-profile directory, in this order: (1)
/// `--profiles-dir`, (2) the `YAGURA_PROFILES_DIR` environment variable (an
/// empty value counts as unset), (3) the `profiles` directory under
/// `namespace`'s config root.
///
/// As in [`resolve_detection_dir`], an explicit stage (1)/(2) answer is
/// never second-guessed: a directory that does not exist, or a path that is
/// a file, is still what that stage returns, and the mismatch is left for
/// `crate::profile::list_profiles` to report as a warning.
///
/// The caller supplies all three path inputs, so this function reads
/// neither the environment nor the filesystem.
#[must_use]
pub fn resolve_profiles_dir(
    arg: Option<PathBuf>,
    env: Option<std::ffi::OsString>,
    config_base: Option<PathBuf>,
    namespace: &InstanceNamespace,
) -> ProfilesDirResolution {
    if let Some(arg) = arg {
        return ProfilesDirResolution::Explicit(arg);
    }
    if let Some(env) = env.filter(|value| !value.is_empty()) {
        return ProfilesDirResolution::Explicit(PathBuf::from(env));
    }
    match config_base {
        Some(base) => ProfilesDirResolution::ConfigRoot(
            yagura_os::instance_dir(&base, namespace).join("profiles"),
        ),
        None => ProfilesDirResolution::Unresolved,
    }
}

/// Resolves the role table path: (1) `--roles`, (2) the `YAGURA_ROLES`
/// environment variable (an empty value counts as unset), (3) the
/// `roles.toml` file under `namespace`'s config root -- the same layout
/// `seed_config_file` seeds the default playbook into.
///
/// Unlike [`resolve_detection_dir`], stage (3) is returned whether or not
/// `roles.toml` already exists on disk: the caller may be about to create it
/// (`crate::roles::save_roles`).
///
/// The caller supplies all three path inputs, so this function reads
/// neither the environment nor the filesystem.
#[must_use]
pub fn resolve_roles_path(
    arg: Option<PathBuf>,
    env: Option<OsString>,
    config_base: Option<PathBuf>,
    namespace: &InstanceNamespace,
) -> Option<PathBuf> {
    if let Some(arg) = arg {
        return Some(arg);
    }
    if let Some(env) = env.filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(env));
    }
    Some(yagura_os::instance_dir(&config_base?, namespace).join("roles.toml"))
}

/// Resolves a window ID from the `YAGURA_WINDOW_ID` environment variable.
///
/// Unlike [`resolve_playbook`], which treats an empty value as unset, this
/// function returns `Some("")` for empty strings to fail-closed: an empty
/// window_id will not resolve to any window and will cause window_not_found on
/// the daemon side, the same as any other id naming no window.
/// Non-UTF-8 values are converted with `to_string_lossy`, which also produces
/// an invalid id that will not match any window (fail-closed).
pub fn resolve_emit_window_id(env: Option<std::ffi::OsString>) -> Option<String> {
    env.map(|value| value.to_string_lossy().into_owned())
}

/// Parses a `WindowStatus` from its wire spelling.
///
/// Deserialization is used rather than a second `match` table so that the
/// accepted spellings stay defined once, by `WindowStatus`'s own serde
/// representation in `yagura-protocol`.
fn parse_status(value: &str) -> Option<WindowStatus> {
    let deserializer: serde::de::value::StrDeserializer<'_, serde::de::value::Error> =
        value.into_deserializer();
    WindowStatus::deserialize(deserializer).ok()
}

fn next_value<'a>(
    remaining: &mut std::slice::Iter<'a, String>,
    flag: &str,
) -> Result<&'a String, CliError> {
    remaining.next().ok_or_else(|| CliError::MissingValue {
        flag: flag.to_string(),
    })
}

fn invalid(flag: &str, value: &str) -> CliError {
    CliError::InvalidValue {
        flag: flag.to_string(),
        value: value.to_string(),
    }
}

/// Resolves `YAGURA_INSTANCE` into the [`InstanceNamespace`] every command -- `Run`,
/// `--shutdown`, `--emit-status` -- must have before anything namespace-derived (the
/// endpoint name, the log directory) can be decided.
///
/// Deliberately asymmetric with [`resolve_playbook`], which treats an empty environment
/// value as unset: an empty `YAGURA_INSTANCE` must not round silently to the default
/// instance, the same asymmetry [`resolve_emit_window_id`] already
/// documents relative to `resolve_playbook`. This function holds no rule of its own -- it
/// forwards straight to `yagura_os::resolve_instance_from`, which is what closes that class
/// (an empty string fails validation there rather than being special-cased here).
///
/// # Errors
///
/// Returns [`InstanceIdError`] for a rejected identifier.
pub fn resolve_instance_id(
    instance_id: Option<OsString>,
) -> Result<InstanceNamespace, InstanceIdError> {
    yagura_os::resolve_instance_from(instance_id.as_deref())
}
