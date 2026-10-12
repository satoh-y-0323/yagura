//! The base environment every child the daemon starts is built on.
//!
//! A daemon started outside a terminal (Finder, Dock, a desktop launcher)
//! inherits a minimal environment: no locale and a system-only `PATH`. A
//! terminal emulator would hand its children the user's login-shell
//! environment and a locale instead, so the daemon decides once at start-up
//! what to lay over its own environment ([`ChildBaseEnv`]). The PTY spawn
//! path, the one-shot capture path and executable resolution all derive from
//! that one value.
//!
//! The rules are pure functions: the host, the inherited environment and every
//! existence check arrive as arguments, so they are testable on any host.
//! [`read_login_shell_env`] runs the login shell itself, and
//! [`resolve_child_base_env`] ties the rules and the read together once at
//! start-up.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use yagura_os::program::ProgramLookup;
use yagura_os::{CaptureLimits, MarkerOutcome};

use crate::config::inherits_terminal;
use crate::profile::ProfileOs;
use crate::pty::{CaptureChildEnv, spawn_one_shot};

/// Prefix reserved for environment variables the daemon itself sets or
/// reads (`YAGURA_STATUS_TOKEN`, `YAGURA_WINDOW_ID`, `YAGURA_INSTANCE`, ...).
///
/// Neither a profile's `[env]` nor a login shell's rc files may supply such a
/// name: either would let configuration outside the daemon impersonate its
/// own injection or redirect a child to another daemon.
pub const RESERVED_ENV_PREFIX: &str = "YAGURA_";

const START_MARKER_TEXT: &str = "[yagura-login-env-start]";
const END_MARKER_TEXT: &str = "[yagura-login-env-end]";

/// The bytes the login-shell read prints right before `env -0`'s output.
pub const LOGIN_ENV_START_MARKER: &[u8] = START_MARKER_TEXT.as_bytes();

/// The bytes the login-shell read prints right after `env -0`'s output.
///
/// Only an occurrence at an item boundary (right after the start marker or
/// right after a NUL) counts as the end. There `env -0` can only be starting
/// a variable name, and a leading `[` plus the hyphens make this text a name
/// no shell exports, so a variable's name or value cannot end the read early.
pub const LOGIN_ENV_END_MARKER: &[u8] = END_MARKER_TEXT.as_bytes();

/// Variables read back from the login shell that describe the read itself or
/// the terminal rather than the user's environment, so the daemon's own
/// values are kept: the shell depth, the read's working directory, the last
/// command run, the terminal type and a stale window size.
const EXCLUDED_LOGIN_ENV_NAMES: [&str; 7] =
    ["SHLVL", "PWD", "OLDPWD", "_", "TERM", "COLUMNS", "LINES"];

/// The variables that declare a locale, any one of which being non-empty
/// means a locale is already chosen.
const LOCALE_VARIABLES: [&str; 3] = ["LC_ALL", "LC_CTYPE", "LANG"];

/// The variable a supplied locale is written to.
const SUPPLIED_LOCALE_VARIABLE: &str = "LANG";

/// Locales tried after the operating system's preferred one, in order.
const FALLBACK_LOCALES: [&str; 2] = ["en_US.UTF-8", "C.UTF-8"];

/// The shell used when neither `$SHELL` nor the password database names one.
const FALLBACK_LOGIN_SHELL: &str = "/bin/sh";

/// Shells that accept `-l` only as their sole argument, so the read runs them
/// interactive but not as a login shell.
const INTERACTIVE_ONLY_SHELLS: [&str; 2] = ["csh", "tcsh"];

/// Environment variables laid over the daemon's own environment for every
/// child it starts: the login shell's environment (when it was read) plus a
/// supplied `LANG` (when no locale was declared).
///
/// A name appears at most once; a later value for the same name replaces an
/// earlier one. `Debug` prints only how many variables there are, never a
/// name or a value, because an environment routinely carries tokens.
#[derive(Clone, Default)]
pub struct ChildBaseEnv {
    vars: Vec<(OsString, OsString)>,
}

impl ChildBaseEnv {
    /// Builds a base environment from `pairs`; for a name given more than
    /// once, the last value wins.
    #[must_use]
    pub fn from_pairs(pairs: Vec<(OsString, OsString)>) -> Self {
        let mut vars: Vec<(OsString, OsString)> = Vec::with_capacity(pairs.len());
        for (name, value) in pairs {
            match vars.iter_mut().find(|(existing, _)| *existing == name) {
                Some(slot) => slot.1 = value,
                None => vars.push((name, value)),
            }
        }
        Self { vars }
    }

    /// The value held for `name`, if any.
    #[must_use]
    pub fn get(&self, name: &OsStr) -> Option<&OsStr> {
        self.vars
            .iter()
            .find(|(existing, _)| existing == name)
            .map(|(_, value)| value.as_os_str())
    }

    /// Every `(name, value)` pair, in the order they are applied to a child.
    pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.vars
            .iter()
            .map(|(name, value)| (name.as_os_str(), value.as_os_str()))
    }

    /// How many variables this holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.vars.len()
    }

    /// Whether this holds no variable, i.e. children see the daemon's own
    /// environment unchanged.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }

    /// The executable-resolution environment a child started with this base
    /// environment sees: each variable is read from here first and from the
    /// daemon's own environment otherwise, so resolution and the spawned
    /// child agree on `PATH`.
    #[must_use]
    pub fn program_lookup(&self) -> ProgramLookup {
        ProgramLookup::from_vars(|name| {
            self.get(OsStr::new(name))
                .map(OsStr::to_os_string)
                .or_else(|| std::env::var_os(name))
        })
    }
}

impl fmt::Debug for ChildBaseEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChildBaseEnv")
            .field("len", &self.vars.len())
            .finish()
    }
}

/// Whether `name` starts with [`RESERVED_ENV_PREFIX`], ASCII
/// case-insensitively.
///
/// Case-insensitive because a Windows environment compares names without
/// regard to case, so a byte-exact check would let a lowercase name overwrite
/// the daemon's own injection. Compares raw bytes, so a name that is not valid
/// UTF-8 is simply not reserved.
#[must_use]
pub fn is_reserved_env_name(name: &OsStr) -> bool {
    name.as_encoded_bytes()
        .get(..RESERVED_ENV_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(RESERVED_ENV_PREFIX.as_bytes()))
}

/// Whether the daemon reads its login shell's environment at start-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginEnvDecision {
    /// Read it: a Unix daemon started without a terminal.
    Read,
    /// Skip it: the daemon inherited a terminal's environment already.
    SkipTerminal,
    /// Skip it: Windows programs neither use `LANG` nor lose `PATH` when
    /// started from the desktop.
    SkipWindows,
}

impl LoginEnvDecision {
    /// The word logged for a skipped read (`terminal` / `windows`), or `None`
    /// for [`LoginEnvDecision::Read`].
    #[must_use]
    pub fn skip_reason(self) -> Option<&'static str> {
        match self {
            Self::Read => None,
            Self::SkipTerminal => Some("terminal"),
            Self::SkipWindows => Some("windows"),
        }
    }
}

/// Decides whether to read the login shell's environment, given the host and
/// the daemon's own inherited `TERM`.
#[must_use]
pub fn login_env_decision(host_os: ProfileOs, term: Option<&OsStr>) -> LoginEnvDecision {
    if host_os == ProfileOs::Windows {
        LoginEnvDecision::SkipWindows
    } else if inherits_terminal(term) {
        LoginEnvDecision::SkipTerminal
    } else {
        LoginEnvDecision::Read
    }
}

/// The argv that makes `shell` print its login environment between
/// [`LOGIN_ENV_START_MARKER`] and [`LOGIN_ENV_END_MARKER`].
///
/// The command uses only absolute paths to external programs joined by `;`,
/// so aliases, functions and each shell's own `printf` cannot change what
/// runs, and every supported shell parses it the same way. `env -0`
/// NUL-separates the variables so values holding newlines survive. The csh
/// family is recognised by the file name of `shell`.
#[must_use]
pub fn login_shell_argv(shell: &str) -> Vec<String> {
    let command = format!(
        "/usr/bin/printf '{START_MARKER_TEXT}'; /usr/bin/env -0; /usr/bin/printf '{END_MARKER_TEXT}'"
    );
    let is_interactive_only = Path::new(shell)
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| INTERACTIVE_ONLY_SHELLS.contains(&name));
    if is_interactive_only {
        vec![shell.to_string(), "-ic".to_string(), command]
    } else {
        vec![
            shell.to_string(),
            "-l".to_string(),
            "-i".to_string(),
            "-c".to_string(),
            command,
        ]
    }
}

/// Extracts the variables the login shell printed, ignoring whatever its rc
/// files wrote before or after them.
///
/// The variables are the `NAME=VALUE` items between the first start marker
/// and the last end marker found at an item boundary. An item with no `=` or
/// an empty name is dropped; values are kept byte for byte. `None` when
/// either marker is missing.
#[must_use]
pub fn parse_login_env(stdout: &[u8]) -> Option<Vec<(OsString, OsString)>> {
    let body_start =
        find_subslice(stdout, LOGIN_ENV_START_MARKER)?.checked_add(LOGIN_ENV_START_MARKER.len())?;
    let body = stdout.get(body_start..)?;
    let body_end = last_end_marker_at_item_boundary(body)?;
    let items = body.get(..body_end)?;
    Some(
        items
            .split(|&byte| byte == 0)
            .filter_map(split_item)
            .collect(),
    )
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn last_end_marker_at_item_boundary(body: &[u8]) -> Option<usize> {
    let last_start = body.len().checked_sub(LOGIN_ENV_END_MARKER.len())?;
    (0..=last_start).rev().find(|&at| {
        let at_boundary = at == 0 || body.get(at - 1) == Some(&0);
        at_boundary
            && body
                .get(at..)
                .is_some_and(|rest| rest.starts_with(LOGIN_ENV_END_MARKER))
    })
}

fn split_item(item: &[u8]) -> Option<(OsString, OsString)> {
    let equals = item.iter().position(|&byte| byte == b'=')?;
    let name = item.get(..equals).filter(|name| !name.is_empty())?;
    let value = item.get(equals + 1..)?;
    Some((os_string_from_bytes(name), os_string_from_bytes(value)))
}

#[cfg(unix)]
fn os_string_from_bytes(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt;
    OsStr::from_bytes(bytes).to_os_string()
}

// The login-shell read never runs off Unix; this only keeps the parser
// testable on every host.
#[cfg(not(unix))]
fn os_string_from_bytes(bytes: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(bytes).into_owned())
}

/// Drops the read-back variables the daemon keeps its own values for: those
/// describing the read or the terminal, and every reserved `YAGURA_` name.
#[must_use]
pub fn filter_login_env(vars: Vec<(OsString, OsString)>) -> Vec<(OsString, OsString)> {
    vars.into_iter()
        .filter(|(name, _)| {
            !is_reserved_env_name(name)
                && !EXCLUDED_LOGIN_ENV_NAMES
                    .iter()
                    .any(|excluded| name == OsStr::new(excluded))
        })
        .collect()
}

/// The environment handed to the login shell: the daemon's inherited
/// environment without the reserved `YAGURA_` names, in the same order, so
/// the rc files see what a terminal's login shell would see.
#[must_use]
pub fn env_for_login_shell(inherited: Vec<(OsString, OsString)>) -> Vec<(OsString, OsString)> {
    inherited
        .into_iter()
        .filter(|(name, _)| !is_reserved_env_name(name))
        .collect()
}

/// Where the shell used for the login-environment read came from; only this
/// word is logged, never the shell's path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginShellSource {
    /// The daemon's `$SHELL`.
    Env,
    /// The user's login shell in the password database.
    Passwd,
    /// `/bin/sh`.
    Fallback,
}

impl LoginShellSource {
    /// The logged word: `env`, `passwd` or `fallback`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Passwd => "passwd",
            Self::Fallback => "fallback",
        }
    }
}

/// Chooses the shell for the login-environment read: `$SHELL`, then the
/// password database's login shell, then `/bin/sh`, taking the first that is
/// an absolute path `exists` accepts. `None` when none qualifies.
#[must_use]
pub fn select_login_shell(
    shell_env: Option<&Path>,
    passwd_shell: Option<&Path>,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<(PathBuf, LoginShellSource)> {
    // `has_root` rather than `is_absolute`: the candidates are POSIX paths,
    // which a Windows host's `is_absolute` rejects for lacking a drive.
    let usable = |path: &Path| path.has_root() && exists(path);
    [
        (shell_env, LoginShellSource::Env),
        (passwd_shell, LoginShellSource::Passwd),
        (
            Some(Path::new(FALLBACK_LOGIN_SHELL)),
            LoginShellSource::Fallback,
        ),
    ]
    .into_iter()
    .find_map(|(candidate, source)| {
        candidate
            .filter(|path| usable(path))
            .map(|path| (path.to_path_buf(), source))
    })
}

/// The `LANG` to supply, or `None` when a locale is already declared or no
/// candidate exists.
///
/// A locale counts as declared when `LC_ALL`, `LC_CTYPE` or `LANG` is
/// non-empty as the child will see it: `base` shadows `inherited`, so an
/// empty value in `base` hides an inherited one. The candidates are
/// `preferred`, `en_US.UTF-8` and `C.UTF-8`, and the first one `exists`
/// accepts wins, since a name the system lacks would make the child's shell
/// warn and fall back to `C` anyway.
#[must_use]
pub fn locale_to_supply(
    base: &[(OsString, OsString)],
    inherited: &dyn Fn(&OsStr) -> Option<OsString>,
    preferred: Option<&str>,
    exists: &dyn Fn(&str) -> bool,
) -> Option<OsString> {
    match decide_locale(base, inherited, preferred, exists) {
        LocaleSupply::Supplied(locale) => Some(locale),
        LocaleSupply::Declared | LocaleSupply::Unavailable => None,
    }
}

/// What [`locale_to_supply`]'s rule decided, kept apart from its `Option` so
/// start-up can log a missing candidate differently from a declared locale.
enum LocaleSupply {
    Declared,
    Supplied(OsString),
    Unavailable,
}

fn decide_locale(
    base: &[(OsString, OsString)],
    inherited: &dyn Fn(&OsStr) -> Option<OsString>,
    preferred: Option<&str>,
    exists: &dyn Fn(&str) -> bool,
) -> LocaleSupply {
    let declared = LOCALE_VARIABLES.iter().any(|variable| {
        let name = OsStr::new(variable);
        let seen = match base.iter().rev().find(|(existing, _)| existing == name) {
            Some((_, value)) => Some(value.clone()),
            None => inherited(name),
        };
        seen.is_some_and(|value| !value.is_empty())
    });
    if declared {
        return LocaleSupply::Declared;
    }
    preferred
        .into_iter()
        .chain(FALLBACK_LOCALES)
        .find(|candidate| exists(candidate))
        .map_or(LocaleSupply::Unavailable, |locale| {
            LocaleSupply::Supplied(OsString::from(locale))
        })
}

/// Composes the base environment from the login shell's read-back variables
/// (`None` when the read was skipped or failed) and the locale rule.
///
/// Always empty on Windows. Elsewhere it is the filtered read-back plus a
/// supplied `LANG` when [`locale_to_supply`] finds one; the locale is
/// supplied even without a read-back.
#[must_use]
pub fn compose_child_base_env(
    host_os: ProfileOs,
    login_vars: Option<Vec<(OsString, OsString)>>,
    inherited: &dyn Fn(&OsStr) -> Option<OsString>,
    preferred: Option<&str>,
    exists: &dyn Fn(&str) -> bool,
) -> ChildBaseEnv {
    compose_with_locale(host_os, login_vars, inherited, preferred, exists).0
}

/// [`compose_child_base_env`], also returning what the locale rule decided
/// (`None` on Windows, where it does not apply).
fn compose_with_locale(
    host_os: ProfileOs,
    login_vars: Option<Vec<(OsString, OsString)>>,
    inherited: &dyn Fn(&OsStr) -> Option<OsString>,
    preferred: Option<&str>,
    exists: &dyn Fn(&str) -> bool,
) -> (ChildBaseEnv, Option<LocaleSupply>) {
    if host_os == ProfileOs::Windows {
        return (ChildBaseEnv::default(), None);
    }
    let mut vars = login_vars.map(filter_login_env).unwrap_or_default();
    let locale = decide_locale(&vars, inherited, preferred, exists);
    if let LocaleSupply::Supplied(value) = &locale {
        vars.push((OsString::from(SUPPLIED_LOCALE_VARIABLE), value.clone()));
    }
    (ChildBaseEnv::from_pairs(vars), Some(locale))
}

/// The largest output the login-shell read accepts. The environment itself
/// cannot exceed the kernel's `exec` limit (1-2 MiB), so this leaves room for
/// whatever the rc files print around it.
pub const LOGIN_ENV_MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// What a successful login-shell read produced. `Debug` never shows a name or
/// a value.
pub struct LoginShellEnv {
    /// Every variable the shell printed, before [`filter_login_env`].
    pub vars: Vec<(OsString, OsString)>,
    /// The shell's exit code, when it exited on its own with one.
    pub exit_code: Option<i32>,
    /// Whether the shell exited on its own (running its logout processing)
    /// before its process tree was ended.
    pub shell_exited: bool,
}

impl fmt::Debug for LoginShellEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginShellEnv")
            .field("vars_len", &self.vars.len())
            .field("exit_code", &self.exit_code)
            .field("shell_exited", &self.shell_exited)
            .finish()
    }
}

/// Why a login-shell read produced no variables. Every variant leaves the
/// children on the daemon's own environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LoginEnvError {
    /// The shell could not be started (missing, not executable, or a path
    /// that is not valid UTF-8).
    #[error("the login shell could not be started")]
    SpawnFailed,
    /// Neither the end marker nor the shell's exit arrived within the
    /// timeout; the shell's process tree was ended.
    #[error("the login shell did not print its environment in time")]
    TimedOut,
    /// The shell printed more than [`LOGIN_ENV_MAX_OUTPUT_BYTES`] before the
    /// end marker; the shell's process tree was ended.
    #[error("the login shell printed more output than accepted")]
    OutputTooLarge,
    /// The shell's output held no complete, parsable environment.
    #[error("the login shell's output held no environment")]
    NoMarkers,
}

impl LoginEnvError {
    /// The word logged for this failure: `spawn_failed`, `timed_out`,
    /// `output_too_large` or `no_markers`.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::SpawnFailed => "spawn_failed",
            Self::TimedOut => "timed_out",
            Self::OutputTooLarge => "output_too_large",
            Self::NoMarkers => "no_markers",
        }
    }
}

/// Runs `shell` as a login shell with exactly `env` as its environment and
/// reads back the environment its rc files leave, via [`login_shell_argv`].
///
/// The shell starts in `env`'s `HOME` when that is an existing directory (the
/// daemon's own working directory otherwise), with no terminal and in a
/// session of its own. The read completes as soon as the end marker arrives,
/// so a background process an rc file started cannot hold it up; the shell
/// then gets the rest of `limits.timeout` to run its logout processing before
/// its process tree is ended. A shell that exits with a non-zero code still
/// counts as a success when its output parses.
///
/// # Errors
///
/// [`LoginEnvError::SpawnFailed`] when the shell cannot be started,
/// [`LoginEnvError::TimedOut`] / [`LoginEnvError::OutputTooLarge`] when
/// `limits` is hit first, and [`LoginEnvError::NoMarkers`] when the output
/// holds no parsable environment.
pub async fn read_login_shell_env(
    shell: &Path,
    env: Vec<(OsString, OsString)>,
    limits: CaptureLimits,
) -> Result<LoginShellEnv, LoginEnvError> {
    let shell = shell.to_str().ok_or(LoginEnvError::SpawnFailed)?;
    let argv = login_shell_argv(shell);
    let cwd = env
        .iter()
        .rev()
        .find(|(name, _)| name == OsStr::new("HOME"))
        .map(|(_, home)| PathBuf::from(home))
        .filter(|home| home.is_dir());
    let child = spawn_one_shot(argv, cwd, CaptureChildEnv::Replaced(env), None)
        .await
        .map_err(|_| LoginEnvError::SpawnFailed)?;
    // Only a NUL-preceded end marker completes the read: `env -0` ends every
    // item with a NUL and no value can hold one, so quoting the marker text in
    // a value cannot end the read early.
    let completion: Vec<u8> = [&[0u8][..], LOGIN_ENV_END_MARKER].concat();
    // A join error means the collecting task ended without an outcome, so
    // no environment was read.
    let outcome = tokio::task::spawn_blocking(move || child.collect_until(limits, &completion))
        .await
        .map_err(|_| LoginEnvError::NoMarkers)?;
    match outcome {
        MarkerOutcome::Reached {
            stdout,
            child_exited,
            code,
            ..
        } => parse_login_env(&stdout)
            .map(|vars| LoginShellEnv {
                vars,
                exit_code: code,
                shell_exited: child_exited,
            })
            .ok_or(LoginEnvError::NoMarkers),
        // An empty environment prints no NUL before the end marker, so the
        // completion needle never appears; the output is whole once the shell
        // has exited.
        MarkerOutcome::Exited { code, stdout, .. } => parse_login_env(&stdout)
            .map(|vars| LoginShellEnv {
                vars,
                exit_code: code,
                shell_exited: true,
            })
            .ok_or(LoginEnvError::NoMarkers),
        MarkerOutcome::TimedOut => Err(LoginEnvError::TimedOut),
        MarkerOutcome::OutputTooLarge => Err(LoginEnvError::OutputTooLarge),
    }
}

/// Everything [`resolve_child_base_env`] reads from the process and the
/// system, gathered by the caller so the resolution itself is testable.
///
/// `Debug` shows which inputs are present and how many variables were
/// inherited, never a path, a name or a value.
pub struct ChildBaseEnvInputs {
    /// The host the daemon runs on.
    pub host_os: ProfileOs,
    /// The daemon's own inherited `TERM`.
    pub term: Option<OsString>,
    /// The daemon's own `$SHELL`.
    pub shell_env: Option<PathBuf>,
    /// The login shell the password database records for the user.
    pub passwd_shell: Option<PathBuf>,
    /// Whether a candidate shell path can be run.
    pub shell_exists: fn(&Path) -> bool,
    /// The daemon's whole inherited environment, in order.
    pub inherited: Vec<(OsString, OsString)>,
    /// The UTF-8 locale name of the user's preferred locale, if the system
    /// reports one.
    pub preferred_locale: Option<String>,
    /// Whether a locale name is installed.
    pub locale_exists: fn(&str) -> bool,
    /// The limits the login-shell read runs under.
    pub limits: CaptureLimits,
}

impl fmt::Debug for ChildBaseEnvInputs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChildBaseEnvInputs")
            .field("host_os", &self.host_os)
            .field("has_term", &self.term.is_some())
            .field("has_shell_env", &self.shell_env.is_some())
            .field("has_passwd_shell", &self.passwd_shell.is_some())
            .field("inherited_len", &self.inherited.len())
            .field("has_preferred_locale", &self.preferred_locale.is_some())
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

/// Decides the daemon's [`ChildBaseEnv`] once at start-up: whether to read
/// the login shell ([`login_env_decision`]), which shell
/// ([`select_login_shell`]), the read itself (given [`env_for_login_shell`]
/// of the inherited environment) and the composition
/// ([`compose_child_base_env`]).
///
/// Never fails: a skipped or failed read leaves only the locale rule, so the
/// daemon starts with its own environment as before. Each outcome is logged
/// by the words that identify it (the skip reason, the shell's source, the
/// failure kind, the variable count); a path, a variable name or a value is
/// never logged.
pub async fn resolve_child_base_env(inputs: ChildBaseEnvInputs) -> ChildBaseEnv {
    let login_vars = read_for_start_up(&inputs).await;
    let ChildBaseEnvInputs {
        host_os,
        inherited,
        preferred_locale,
        locale_exists,
        ..
    } = inputs;
    // The locale check loads locale data from disk, so it stays off the
    // async workers like the passwd lookup does.
    let composed = tokio::task::spawn_blocking(move || {
        let inherited = |name: &OsStr| {
            inherited
                .iter()
                .rev()
                .find(|(existing, _)| existing == name)
                .map(|(_, value)| value.clone())
        };
        compose_with_locale(
            host_os,
            login_vars,
            &inherited,
            preferred_locale.as_deref(),
            &|name| locale_exists(name),
        )
    })
    .await;
    let (base, locale) = match composed {
        Ok(composed) => composed,
        Err(_) => {
            tracing::warn!(
                "composing the child base environment failed; children inherit the daemon's own"
            );
            return ChildBaseEnv::default();
        }
    };
    match locale {
        Some(LocaleSupply::Supplied(value)) => tracing::info!(
            "no locale is declared; children get LANG={}",
            value.to_string_lossy()
        ),
        Some(LocaleSupply::Unavailable) => tracing::warn!(
            "no locale is declared and no candidate locale is installed; \
             children start without one"
        ),
        Some(LocaleSupply::Declared) | None => {}
    }
    base
}

/// The read-back variables for [`resolve_child_base_env`], or `None` when
/// the read is skipped or fails; logs which of those happened.
async fn read_for_start_up(inputs: &ChildBaseEnvInputs) -> Option<Vec<(OsString, OsString)>> {
    if let Some(reason) = login_env_decision(inputs.host_os, inputs.term.as_deref()).skip_reason() {
        tracing::info!("login shell environment not read: reason={reason}");
        return None;
    }
    let shell_exists = inputs.shell_exists;
    let Some((shell, source)) = select_login_shell(
        inputs.shell_env.as_deref(),
        inputs.passwd_shell.as_deref(),
        &|path| shell_exists(path),
    ) else {
        tracing::info!("login shell environment not read: reason=no_shell");
        return None;
    };
    let source = source.as_str();
    let started = Instant::now();
    let env = env_for_login_shell(inputs.inherited.clone());
    match read_login_shell_env(&shell, env, inputs.limits).await {
        Ok(read) => {
            tracing::info!(
                "login shell environment read: source={source} variables={} elapsed_ms={}",
                read.vars.len(),
                started.elapsed().as_millis()
            );
            match (read.shell_exited, read.exit_code) {
                (true, Some(0)) => {}
                (true, code) => tracing::warn!(
                    "the login shell exited with code {code:?}; its environment is used anyway"
                ),
                (false, _) => tracing::warn!(
                    "the login shell did not exit after printing its environment; \
                     its process tree was ended and its environment is used anyway"
                ),
            }
            Some(read.vars)
        }
        Err(error) => {
            tracing::warn!(
                "login shell environment not read: kind={} source={source} elapsed_ms={}; \
                 children keep the daemon's own environment",
                error.kind(),
                started.elapsed().as_millis()
            );
            None
        }
    }
}
