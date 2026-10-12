//! The instance namespace: the single name every per-instance resource of
//! yagura is derived from.
//!
//! One yagura instance owns one IPC endpoint plus one config / state / data
//! directory, and all four names are composed from the namespace held here.
//! The namespace is `yagura` by default and `yagura-<id>` when the user names
//! an instance, so a named instance shares no resource with the default one.
//! On Windows the directories use the app's bundle identifier in place of
//! `yagura` ([`instance_dir`]).
//!
//! [`resolve_instance_from`] is the only way to obtain an
//! [`InstanceNamespace`]: the type has no public constructor, no `Default`
//! and no deserializer, so a namespace value cannot exist without having
//! passed the identifier validation below. The composed name is never handed
//! out as a string either; every path and endpoint name that contains it is
//! assembled inside this crate ([`instance_dir`], `default_endpoint`,
//! `default_endpoint_owning`, `seed_config_file`).
//!
//! This module reads no environment variable. Values arrive as arguments and
//! the variable *names* stay with the caller.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The namespace of the default instance, and the prefix every named
/// instance's namespace is composed from.
const DEFAULT_NAMESPACE: &str = "yagura";

/// The app's bundle identifier, matching `identifier` in the GUI's
/// `tauri.conf.json`.
pub const APP_IDENTIFIER: &str = "io.github.satoh-y-0323.yagura";

/// The default instance's directory under the platform's base directory, and
/// the prefix of every named instance's. On Windows the installer puts the
/// program in `%LOCALAPPDATA%\yagura`, and its "delete app data" option
/// removes `%LOCALAPPDATA%\<identifier>`, so the data lives there instead.
#[cfg(windows)]
const DEFAULT_DIR_NAME: &str = APP_IDENTIFIER;
#[cfg(not(windows))]
const DEFAULT_DIR_NAME: &str = DEFAULT_NAMESPACE;

/// Upper bound on an instance identifier, in bytes.
///
/// The real safety net for path length is `build_endpoint`'s
/// `sockaddr_un.sun_path` check on the resolved socket path; this bound only
/// keeps identifiers short enough that the check is never the first thing a
/// user meets. It is sized against the Unix stage-3 fallback under macOS's
/// `TMPDIR` (`/var/folders/xx/<28 chars>/T/`, 49 bytes), the longest common
/// root: `yagura-<id>-<user_id>/ipc.sock` then stays well inside the 104-byte
/// limit.
const MAX_INSTANCE_ID_LEN: usize = 16;

/// The namespace one yagura instance owns: `yagura` for the default instance
/// and `yagura-<id>` for a named one.
///
/// Obtained only through [`resolve_instance_from`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceNamespace {
    /// Invariant: `DEFAULT_NAMESPACE`, or `DEFAULT_NAMESPACE-<validated id>`.
    name: String,
    /// Invariant: `DEFAULT_DIR_NAME`, or `DEFAULT_DIR_NAME-<validated id>`.
    dir_name: String,
    id: Option<String>,
}

impl InstanceNamespace {
    /// Whether this is the default (unnamed) instance.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.id.is_none()
    }

    /// The identifier the user gave, for display only; [`None`] for the
    /// default instance.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// The composed namespace name. Crate-private on purpose: keeping the
    /// name out of callers' hands is what makes "every per-instance name goes
    /// through this crate" a property of the type rather than a convention.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

/// Why an instance identifier was rejected.
///
/// The rejected value itself is deliberately not carried: these messages
/// reach stderr, logs and the GUI, and an identifier that came from the
/// environment is attacker-influenced input that must not be echoed back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InstanceIdError {
    /// The identifier was empty.
    #[error("instance identifier is empty; give at least one character or unset it entirely")]
    Empty,
    /// The identifier was longer than the allowed number of bytes.
    #[error("instance identifier is longer than {MAX_INSTANCE_ID_LEN} bytes")]
    TooLong,
    /// The identifier held something outside the allow-list.
    #[error(
        "instance identifier may only contain lowercase ASCII letters, digits and '-' \
         (uppercase, whitespace, path separators and non-ASCII characters are rejected)"
    )]
    DisallowedCharacter,
    /// The identifier started or ended with a hyphen.
    #[error("instance identifier must start and end with a lowercase ASCII letter or a digit")]
    HyphenAtBoundary,
}

/// Why a start-up that opens the store was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StartupEnvError {
    /// The instance identifier did not pass [`resolve_instance_from`].
    #[error("invalid instance identifier: {0}")]
    InvalidInstanceId(#[from] InstanceIdError),
    /// A named instance was combined with a store-path override.
    #[error(
        "a store path override cannot be combined with a named instance: both instances would \
         open the same database and the one starting later would drop the other's previous \
         session. Unset the override to start this instance"
    )]
    StoreOverrideWithNamedInstance,
}

/// Resolves an instance identifier into its namespace. The only way to obtain
/// an [`InstanceNamespace`], so every namespace value has passed this check.
///
/// `instance_id = None` yields the default instance, whose composed name is
/// unchanged from a yagura that knows nothing about instances.
///
/// An identifier is accepted when it is 1 to 16 bytes of lowercase ASCII
/// letters, ASCII digits and `-`, and starts and ends with a letter or digit.
/// The allow-list is what closes the class rather than a list of forbidden
/// characters: path separators, `.`, `..`, NUL, whitespace, the characters
/// Windows reserves in file names, and every non-ASCII byte are unreachable
/// because they are not in the set. Uppercase is rejected so that two
/// identifiers differing only in case cannot name one endpoint (Windows pipe
/// names are case-insensitive) while naming two different Linux directories.
///
/// # Errors
///
/// Returns [`InstanceIdError`] for an identifier that breaks any of the rules
/// above. There is no fallback to the default instance: a caller that cannot
/// name its instance must stop rather than share the default one's resources.
pub fn resolve_instance_from(
    instance_id: Option<&OsStr>,
) -> Result<InstanceNamespace, InstanceIdError> {
    let Some(candidate) = instance_id else {
        return Ok(InstanceNamespace {
            name: DEFAULT_NAMESPACE.to_owned(),
            dir_name: DEFAULT_DIR_NAME.to_owned(),
            id: None,
        });
    };
    let id = validate_instance_id(candidate)?;
    Ok(InstanceNamespace {
        name: format!("{DEFAULT_NAMESPACE}-{id}"),
        dir_name: format!("{DEFAULT_DIR_NAME}-{id}"),
        id: Some(id.to_owned()),
    })
}

fn validate_instance_id(candidate: &OsStr) -> Result<&str, InstanceIdError> {
    // Nothing outside ASCII is in the allow-list, so a non-UTF-8 identifier
    // fails the same rule the character scan below applies.
    let id = candidate
        .to_str()
        .ok_or(InstanceIdError::DisallowedCharacter)?;
    if id.is_empty() {
        return Err(InstanceIdError::Empty);
    }
    if id.len() > MAX_INSTANCE_ID_LEN {
        return Err(InstanceIdError::TooLong);
    }
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(InstanceIdError::DisallowedCharacter);
    }
    if id.starts_with('-') || id.ends_with('-') {
        return Err(InstanceIdError::HyphenAtBoundary);
    }
    Ok(id)
}

/// Whether a named instance is being asked to use an overridden store path.
///
/// **This rejection is a yagura application policy, not an operating-system
/// constraint**: the platform is perfectly able to open that database. It
/// exists because a store path that does not follow the namespace lets two
/// instances open one database, and the second one to start drops the first
/// one's previous-session snapshot.
///
/// "Configured" means `store_path_override` is [`Some`]. An empty value and a
/// non-UTF-8 value both count as configured, matching the rule the store
/// resolution itself adopts an override by -- a judgment that disagreed with
/// that rule would let one side start and the other refuse.
///
/// The default instance never conflicts: an override there keeps winning
/// exactly as it does today.
#[must_use]
pub fn store_override_conflicts(
    namespace: &InstanceNamespace,
    store_path_override: Option<&OsStr>,
) -> bool {
    !namespace.is_default() && store_path_override.is_some()
}

/// Resolves the namespace for a start-up that opens the store, applying both
/// [`resolve_instance_from`] and [`store_override_conflicts`] at once.
///
/// Commands that never touch the store (shutting a daemon down, reporting its
/// status) call [`resolve_instance_from`] instead, so an override in their
/// environment cannot keep them from addressing a named instance.
///
/// # Errors
///
/// Returns [`StartupEnvError::InvalidInstanceId`] when the identifier is
/// rejected, and [`StartupEnvError::StoreOverrideWithNamedInstance`] when a
/// named instance is combined with a configured store-path override.
pub fn resolve_startup_env(
    instance_id: Option<&OsStr>,
    store_path_override: Option<&OsStr>,
) -> Result<InstanceNamespace, StartupEnvError> {
    let namespace = resolve_instance_from(instance_id)?;
    if store_override_conflicts(&namespace, store_path_override) {
        return Err(StartupEnvError::StoreOverrideWithNamedInstance);
    }
    Ok(namespace)
}

/// The directory this instance owns under `base`: `<base>/yagura` or
/// `<base>/yagura-<id>`, with [`APP_IDENTIFIER`] in place of `yagura` on
/// Windows.
///
/// The one place an instance's directory name is joined to a caller-supplied
/// directory.
#[must_use]
pub fn instance_dir(base: &Path, namespace: &InstanceNamespace) -> PathBuf {
    base.join(&namespace.dir_name)
}
