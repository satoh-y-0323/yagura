//! Reads `client.toml`, the GUI's own client-side settings.
//!
//! The file is never seeded, so an install that never creates it keeps the
//! built-in defaults.
//!
//! The base directory and the environment-variable value are caller-supplied
//! arguments rather than something this module resolves itself, so it needs
//! no `AppHandle` and is reachable from a test with a plain temp directory.

use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// File name of the client settings, resolved under the caller-supplied base
/// directory (in production, `config_paths::config_root`).
const CLIENT_CONFIG_FILE_NAME: &str = "client.toml";

/// Read limit for `client.toml`: a hand-written config has no legitimate
/// reason to exceed it. Enforced during the read, since another process
/// running as the same user could grow the file.
const MAX_CLIENT_CONFIG_BYTES: u64 = 64 * 1024;

/// Default for [`ReconnectConfig::auto_respawn`].
const DEFAULT_AUTO_RESPAWN: bool = true;

/// Default for [`ReconnectConfig::initial_delay_ms`].
const DEFAULT_INITIAL_DELAY_MS: u64 = 1000;

/// Default for [`ReconnectConfig::backoff_factor`].
const DEFAULT_BACKOFF_FACTOR: u32 = 2;

/// Default for [`ReconnectConfig::attempt_limit`].
const DEFAULT_ATTEMPT_LIMIT: u32 = 3;

/// Smallest accepted [`ReconnectConfig::initial_delay_ms`].
const MIN_INITIAL_DELAY_MS: u64 = 100;

/// Largest accepted [`ReconnectConfig::initial_delay_ms`].
const MAX_INITIAL_DELAY_MS: u64 = 60_000;

/// Smallest accepted [`ReconnectConfig::backoff_factor`].
const MIN_BACKOFF_FACTOR: u32 = 1;

/// Largest accepted [`ReconnectConfig::backoff_factor`].
const MAX_BACKOFF_FACTOR: u32 = 10;

/// Smallest accepted [`ReconnectConfig::attempt_limit`].
const MIN_ATTEMPT_LIMIT: u32 = 1;

/// Largest accepted [`ReconnectConfig::attempt_limit`].
const MAX_ATTEMPT_LIMIT: u32 = 10;

/// Environment-variable values that turn auto-respawn off, compared after
/// trimming and lowercasing the raw value. Any other non-empty (post-trim)
/// value turns it on.
const AUTO_RESPAWN_DISABLING_VALUES: [&str; 2] = ["0", "false"];

/// Why `client.toml` could not be loaded. Serializes as a `snake_case`
/// string across the Tauri command boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientConfigFailure {
    /// No file under the base directory. Not a defect: the file is never
    /// seeded, so most installs keep the defaults.
    NotFound,
    /// The file exists but could not be read as text.
    ReadFailed,
    /// The file's bytes parse as text but not as the expected shape.
    InvalidToml,
    /// The file exceeds the read limit.
    TooLarge,
}

/// The `[reconnect]` settings that drive daemon recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectConfig {
    /// Whether the supervisor may start the daemon on its own.
    pub auto_respawn: bool,
    /// How long to wait after the first attempt, in milliseconds.
    pub initial_delay_ms: u64,
    /// What the wait is multiplied by after each further attempt.
    pub backoff_factor: u32,
    /// How many attempts are made before recovery is given up.
    pub attempt_limit: u32,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            auto_respawn: DEFAULT_AUTO_RESPAWN,
            initial_delay_ms: DEFAULT_INITIAL_DELAY_MS,
            backoff_factor: DEFAULT_BACKOFF_FACTOR,
            attempt_limit: DEFAULT_ATTEMPT_LIMIT,
        }
    }
}

/// Result of a [`load_client_config`] attempt.
///
/// `reconnect` carries the defaults whenever `failure` is `Some`, except for
/// whatever the environment value decides: a file that cannot be understood
/// yields no partial settings (fail-safe), and the environment value is not
/// part of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientConfigLoadResult {
    /// Set when the defaults are in force because loading failed.
    pub failure: Option<ClientConfigFailure>,
    /// The settings in force.
    pub reconnect: ReconnectConfig,
}

/// The shape `client.toml` must deserialize into. Unknown keys are ignored
/// rather than rejected, so a newer file stays readable by an older build.
#[derive(Debug, Default, Deserialize)]
struct ClientConfigFile {
    #[serde(default)]
    reconnect: ReconnectSection,
}

/// Every key is optional: a file that sets some of them keeps the defaults
/// for the rest, which is what makes the resolution per key.
#[derive(Debug, Default, Deserialize)]
struct ReconnectSection {
    auto_respawn: Option<bool>,
    initial_delay_ms: Option<u64>,
    backoff_factor: Option<u32>,
    attempt_limit: Option<u32>,
}

/// Resolves the reconnect settings from the environment-variable value, then
/// `client.toml` under `base_dir`, then the built-in defaults.
///
/// `auto_respawn_env` is the raw value of the auto-respawn variable; an empty
/// or whitespace-only string counts as unset and falls through to the file.
/// It carries no other key, so the remaining three are resolved from the file
/// or the defaults.
///
/// A value outside its accepted range falls back to that field's default
/// without disturbing the other fields and without being reported: only a
/// file that cannot be understood as a whole produces a `failure`.
///
/// Never returns `Err` and never panics: every failure mode is reported
/// through [`ClientConfigLoadResult::failure`] while the defaults stay in
/// force (fail-safe).
pub fn load_client_config(
    base_dir: &Path,
    auto_respawn_env: Option<&str>,
) -> ClientConfigLoadResult {
    let (failure, section) = read_reconnect_section(base_dir);
    let reconnect = apply_auto_respawn_env(
        resolve_reconnect(section.unwrap_or_default()),
        auto_respawn_env,
    );

    ClientConfigLoadResult { failure, reconnect }
}

/// Resolves the reconnect settings when there is no base directory to look
/// in at all, which is the same situation as the file being absent:
/// `NotFound` with the defaults, and the environment value applied exactly
/// as [`load_client_config`] applies it.
pub(crate) fn client_config_without_base_dir(
    auto_respawn_env: Option<&str>,
) -> ClientConfigLoadResult {
    ClientConfigLoadResult {
        failure: Some(ClientConfigFailure::NotFound),
        reconnect: apply_auto_respawn_env(ReconnectConfig::default(), auto_respawn_env),
    }
}

/// Lets the environment value override auto-respawn. An empty or
/// whitespace-only string counts as unset, and any other value other than
/// the disabling ones (compared trimmed and case-insensitively) turns it on.
fn apply_auto_respawn_env(
    reconnect: ReconnectConfig,
    auto_respawn_env: Option<&str>,
) -> ReconnectConfig {
    match auto_respawn_env
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
    {
        Some(value) => ReconnectConfig {
            auto_respawn: !AUTO_RESPAWN_DISABLING_VALUES
                .iter()
                .any(|disabling| value.eq_ignore_ascii_case(disabling)),
            ..reconnect
        },
        None => reconnect,
    }
}

/// Reads and parses the file, yielding the section it declares or the reason
/// nothing could be taken from it. The two are mutually exclusive.
///
/// The byte limit is enforced on the same file handle that is read, so a
/// process growing the file cannot slip past a separate size check (CWE-367).
fn read_reconnect_section(
    base_dir: &Path,
) -> (Option<ClientConfigFailure>, Option<ReconnectSection>) {
    let path = base_dir.join(CLIENT_CONFIG_FILE_NAME);

    // A missing file is a legitimate case (the file is never seeded); every
    // other open error is a read failure.
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (Some(ClientConfigFailure::NotFound), None);
        }
        Err(_) => return (Some(ClientConfigFailure::ReadFailed), None),
    };

    // Bytes that are not valid UTF-8 are a read failure rather than invalid
    // TOML: the parser below never sees content that cannot be decoded.
    let mut limited_reader = file.take(MAX_CLIENT_CONFIG_BYTES);
    let mut content = String::new();
    if limited_reader.read_to_string(&mut content).is_err() {
        return (Some(ClientConfigFailure::ReadFailed), None);
    }

    // One further byte from the underlying handle means the file exceeded the
    // limit; the capped reader itself can never see past it.
    let mut underlying_file = limited_reader.into_inner();
    let mut extra = [0u8; 1];
    if underlying_file.read_exact(&mut extra).is_ok() {
        return (Some(ClientConfigFailure::TooLarge), None);
    }

    match toml::from_str::<ClientConfigFile>(&content) {
        Ok(parsed) => (None, Some(parsed.reconnect)),
        Err(_) => (Some(ClientConfigFailure::InvalidToml), None),
    }
}

/// Applies the file's keys over the defaults, dropping the ones whose value
/// lies outside its accepted range.
fn resolve_reconnect(section: ReconnectSection) -> ReconnectConfig {
    let defaults = ReconnectConfig::default();

    ReconnectConfig {
        auto_respawn: section.auto_respawn.unwrap_or(defaults.auto_respawn),
        initial_delay_ms: within(
            section.initial_delay_ms,
            MIN_INITIAL_DELAY_MS,
            MAX_INITIAL_DELAY_MS,
        )
        .unwrap_or(defaults.initial_delay_ms),
        backoff_factor: within(
            section.backoff_factor,
            MIN_BACKOFF_FACTOR,
            MAX_BACKOFF_FACTOR,
        )
        .unwrap_or(defaults.backoff_factor),
        attempt_limit: within(section.attempt_limit, MIN_ATTEMPT_LIMIT, MAX_ATTEMPT_LIMIT)
            .unwrap_or(defaults.attempt_limit),
    }
}

/// Keeps `value` only when it lies within the inclusive bounds. An
/// out-of-range value is dropped rather than clamped, so the field takes the
/// default instead of a bound the file never asked for.
fn within<T: PartialOrd>(value: Option<T>, min: T, max: T) -> Option<T> {
    value.filter(|candidate| *candidate >= min && *candidate <= max)
}
