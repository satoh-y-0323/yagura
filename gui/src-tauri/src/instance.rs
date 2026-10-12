//! The GUI's own copy of the "instance" fact: which yagura instance this
//! process belongs to, decided once at start-up and carried from then on as
//! a value rather than re-read from the environment.
//!
//! [`StartupContext::resolve_from`] reads no environment variable itself
//! (every input arrives as an argument, matching the `*_from` seam / `*_env`
//! wrapper idiom `yagura_os` already uses); `main.rs`'s `setup` is the only
//! place in this crate that calls it with the process environment, and the
//! result is handed to every command and background task through Tauri's
//! managed state instead of a second env read of their own.

use std::ffi::OsStr;
use std::path::PathBuf;

use serde::Serialize;

/// Why this process could not resolve which instance it belongs to.
///
/// Only two variants: the specific allow-list rule an identifier broke is
/// not carried here, because this value reaches a banner, a log line and the
/// recovery report, none of which should echo attacker-influenced input
/// (the same rule [`yagura_os::InstanceIdError`] already follows for its own
/// `Display`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupEnvFailure {
    /// `YAGURA_INSTANCE` did not pass the identifier allow-list.
    InvalidInstanceId,
    /// A named instance was combined with `YAGURA_STORE_DB`.
    StoreOverrideWithNamedInstance,
}

impl From<yagura_os::StartupEnvError> for StartupEnvFailure {
    fn from(error: yagura_os::StartupEnvError) -> Self {
        match error {
            yagura_os::StartupEnvError::InvalidInstanceId(_) => Self::InvalidInstanceId,
            yagura_os::StartupEnvError::StoreOverrideWithNamedInstance => {
                Self::StoreOverrideWithNamedInstance
            }
        }
    }
}

/// The instance this process resolved to, or why it could not.
pub type InstanceResolution = Result<yagura_os::InstanceNamespace, StartupEnvFailure>;

/// The fact this process decided once at start-up: which instance it
/// belongs to, and the configuration base directory that instance's root is
/// composed under.
///
/// Managed as Tauri state (`.manage()`), the same way `ConnectionPool` and
/// `RecoverySupervisorState` are, rather than a `OnceLock`: a Tauri
/// application already has a place to keep a value decided once and read
/// from many commands, and reusing it means the resolution stays a plain
/// value that tests can construct directly instead of a process-global that
/// only `std::env::set_var` could exercise.
pub struct StartupContext {
    resolution: InstanceResolution,
    config_base: Option<PathBuf>,
}

impl StartupContext {
    /// Resolves the start-up fact from explicit inputs. Reads no
    /// environment variable: `instance_id` and `store_path_override` are
    /// compared against the allow-list and the store-override rule exactly
    /// as `yagura_os::resolve_startup_env` defines them, and `config_base`
    /// is used as given.
    #[must_use]
    pub fn resolve_from(
        instance_id: Option<&OsStr>,
        store_path_override: Option<&OsStr>,
        config_base: Option<PathBuf>,
    ) -> Self {
        let resolution = yagura_os::resolve_startup_env(instance_id, store_path_override)
            .map_err(StartupEnvFailure::from);
        Self {
            resolution,
            config_base,
        }
    }

    /// The resolved instance, or the reason resolution failed.
    pub fn resolution(&self) -> &InstanceResolution {
        &self.resolution
    }

    /// This instance's configuration root: `config_base` joined with the
    /// resolved namespace. `None` when either half is missing, so a failed
    /// resolution never falls back to the default instance's root.
    #[must_use]
    pub fn config_root(&self) -> Option<PathBuf> {
        let base = self.config_base.as_deref()?;
        let namespace = self.resolution.as_ref().ok()?;
        Some(crate::config_paths::config_root(base, namespace))
    }

    /// The `(config_base, config_root)` pair a config write needs: the base
    /// bounds which directories may be created or secured, and the root is
    /// exactly what [`StartupContext::config_root`] returns. `None` in the
    /// same cases as `config_root`.
    #[must_use]
    pub fn config_location(&self) -> Option<(PathBuf, PathBuf)> {
        let root = self.config_root()?;
        let base = self.config_base.clone()?;
        Some((base, root))
    }
}

/// Composes a window title from `stem` (read at run time from the window's
/// current title, never hardcoded here) and the instance identifier, if the
/// instance is not the default one.
#[must_use]
pub fn compose_window_title(stem: &str, id: Option<&str>) -> String {
    match id {
        Some(id) => format!("{stem} [{id}]"),
        None => stem.to_owned(),
    }
}
