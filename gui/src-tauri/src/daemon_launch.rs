//! Starts the daemon process when none is running, and connects to it once
//! it has bound its endpoint.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use yagura_os::program::Platform;
use yagura_os::{DetachedSpawn, OsError, resolve_sibling_executable, spawn_daemon_detached};

use crate::config_paths::resolve_playbook_path;
use crate::constants::{CONNECT_RETRY_INTERVAL, POST_SPAWN_CONNECT_RETRY_LIMIT};
use crate::events::DaemonSpawnWarningPayload;
use crate::instance::StartupContext;
use crate::ipc_client::{Connection, IpcError, connect_retrying_with_limit};

/// Starts the daemon detached, handing `warn` the warning the WebView must
/// show when the platform refused the strongest detachment mode.
///
/// Passes no `--instance` flag: the daemon resolves the same instance this
/// process did by inheriting `YAGURA_INSTANCE` from this process's own
/// environment (`spawn_daemon_detached` does not clear it), so the two never
/// have to be told about each other's resolution explicitly.
///
/// # Errors
/// Returns [`IpcError::DaemonNotFound`] when the daemon executable is not
/// next to this one, and [`IpcError::DaemonSpawn`] when it cannot be started.
pub(crate) fn spawn_daemon(
    startup: &StartupContext,
    warn: impl FnOnce(DaemonSpawnWarningPayload),
) -> Result<(), IpcError> {
    let path = resolve_daemon_path()?;
    let own_exe = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(|_| IpcError::DaemonSpawn)?;
    spawn_daemon_with(&path, &own_exe, startup, spawn_daemon_detached, warn)
}

/// [`spawn_daemon`] with the executable, the GUI's own path and the launcher
/// given, so what is passed to the daemon and what is reported can be read
/// without starting a process.
///
/// When the platform refuses the strongest detachment mode,
/// [`spawn_daemon_detached`] falls back to a weaker one and reports it through
/// `used_fallback`; that fallback is surfaced because the GUI has no log of
/// its own.
fn spawn_daemon_with(
    path: &Path,
    own_exe: &Path,
    startup: &StartupContext,
    launch: impl FnOnce(&Path, &[&OsStr]) -> Result<DetachedSpawn, OsError>,
    warn: impl FnOnce(DaemonSpawnWarningPayload),
) -> Result<(), IpcError> {
    let playbook_path = resolve_playbook_path(startup);
    // The daemon only accepts `register_approver` from the executable named
    // by `--approver-exe`, so the GUI passes its own path there.
    let mut args: Vec<&OsStr> = vec![OsStr::new("--approver-exe"), own_exe.as_os_str()];
    if let Some(playbook_path) = playbook_path.as_deref() {
        args.push(OsStr::new("--playbook"));
        args.push(playbook_path.as_os_str());
    }
    let spawn = launch(path, &args).map_err(|_| IpcError::DaemonSpawn)?;
    if spawn.used_fallback {
        warn(DaemonSpawnWarningPayload {
            message: "The daemon was started without the strongest detachment mode; \n                      it may be terminated together with this window"
                .to_owned(),
        });
    }
    Ok(())
}

/// Resolves the daemon executable next to the GUI executable, through the
/// one `yagura-os` [`resolve_sibling_executable`] every "find the executable
/// sitting next to mine" resolution in the workspace shares,
/// so the containment check (canonicalized, still a direct child of the
/// GUI's own directory, a regular file) is applied by one implementation
/// rather than each caller keeping its own.
fn resolve_daemon_path() -> Result<PathBuf, IpcError> {
    let executable = std::env::current_exe().map_err(|_| IpcError::DaemonNotFound)?;
    let directory = executable.parent().ok_or(IpcError::DaemonNotFound)?;
    resolve_sibling_executable(directory, "yagura-daemon", Platform::host())
        .ok_or(IpcError::DaemonNotFound)
}

/// Connects to a daemon that was just spawned. Unlike
/// [`crate::ipc_client::connect_retrying`] this waits before the first
/// attempt, because the daemon still has to bind its endpoint -- reading the
/// login shell's environment first on Unix, so the budget
/// ([`POST_SPAWN_CONNECT_RETRY_LIMIT`]) is longer than the ordinary one.
pub(crate) async fn connect_after_spawn(endpoint: &str) -> Result<Connection, IpcError> {
    tokio::time::sleep(CONNECT_RETRY_INTERVAL).await;
    connect_retrying_with_limit(endpoint, POST_SPAWN_CONNECT_RETRY_LIMIT)
        .await
        .ok_or(IpcError::ConnectTimeout)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::ffi::OsString;

    use yagura_test_support::paths::TempDir;

    use super::*;

    type Launched = Vec<(PathBuf, Vec<OsString>)>;

    /// Runs `spawn_daemon_with` with a launcher that records what it was given
    /// and answers `launcher`; returns the outcome, the launches and the
    /// warnings.
    fn spawn_recording(
        startup: &StartupContext,
        launcher: Result<DetachedSpawn, OsError>,
    ) -> (Result<(), IpcError>, Launched, Vec<String>) {
        let launched = RefCell::new(Vec::new());
        let warnings = RefCell::new(Vec::new());
        let outcome = spawn_daemon_with(
            Path::new("daemon"),
            Path::new("gui"),
            startup,
            |path, args| {
                launched.borrow_mut().push((
                    path.to_path_buf(),
                    args.iter().map(|arg| (*arg).to_owned()).collect(),
                ));
                launcher
            },
            |warning| warnings.borrow_mut().push(warning.message),
        );
        (outcome, launched.into_inner(), warnings.into_inner())
    }

    fn startup_under(base: &Path) -> StartupContext {
        StartupContext::resolve_from(None, None, Some(base.to_path_buf()))
    }

    fn plain_spawn() -> Result<DetachedSpawn, OsError> {
        Ok(DetachedSpawn {
            used_fallback: false,
        })
    }

    #[test]
    fn the_daemon_is_started_with_the_executable_that_may_register_as_approver() {
        let base = TempDir::new("yagura-test", "launch-no-playbook");

        let (outcome, launched, warnings) =
            spawn_recording(&startup_under(base.path()), plain_spawn());

        assert!(outcome.is_ok(), "got {outcome:?}");
        assert_eq!(
            launched,
            [(
                PathBuf::from("daemon"),
                vec![OsString::from("--approver-exe"), OsString::from("gui")]
            )]
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn the_daemon_is_handed_the_playbook_the_config_root_holds() {
        let base = TempDir::new("yagura-test", "launch-playbook");
        let startup = startup_under(base.path());
        let root = startup
            .config_root()
            .expect("the default instance has a root");
        std::fs::create_dir_all(&root).expect("create the config root");
        let playbook = root.join("playbook.toml");
        std::fs::write(&playbook, "").expect("write the playbook");

        let (outcome, launched, _warnings) = spawn_recording(&startup, plain_spawn());

        assert!(outcome.is_ok(), "got {outcome:?}");
        assert_eq!(
            launched[0].1,
            [
                OsString::from("--approver-exe"),
                OsString::from("gui"),
                OsString::from("--playbook"),
                playbook.into_os_string(),
            ]
        );
    }

    #[test]
    fn a_weaker_detachment_than_asked_for_is_reported_to_the_webview() {
        let base = TempDir::new("yagura-test", "launch-fallback");
        let startup = startup_under(base.path());

        let (outcome, _launched, warnings) = spawn_recording(
            &startup,
            Ok(DetachedSpawn {
                used_fallback: true,
            }),
        );

        assert!(outcome.is_ok(), "got {outcome:?}");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("detachment"), "got {:?}", warnings[0]);
    }

    #[test]
    fn a_daemon_that_could_not_be_launched_is_reported_as_a_failed_start() {
        let base = TempDir::new("yagura-test", "launch-fails");
        let startup = startup_under(base.path());

        let (outcome, _launched, warnings) =
            spawn_recording(&startup, Err(OsError::Security("refused".to_owned())));

        assert!(
            matches!(outcome, Err(IpcError::DaemonSpawn)),
            "got {outcome:?}"
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn the_resolved_daemon_path_is_an_existing_daemon_executable() {
        match resolve_daemon_path() {
            Ok(path) => {
                assert!(path.is_file(), "{path:?} is not a file");
                assert_eq!(path.file_stem(), Some(OsStr::new("yagura-daemon")));
            }
            Err(error) => assert!(matches!(error, IpcError::DaemonNotFound)),
        }
    }
}
