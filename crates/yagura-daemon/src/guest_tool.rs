//! Running one of a profile's `[host_paths]` tools that print a fact about
//! the environment its CLI runs in (`state_base`, `home`): as its own
//! command, in the daemon's empty folder, with the profile's `[env]`, under
//! the launch slots a path conversion takes and the listing time limit.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::Semaphore;
use tokio::time::Instant;

use crate::config::DaemonConfig;
use crate::pty;

/// A `[host_paths]` tool and what it runs with.
pub(crate) struct GuestTool {
    pub(crate) argv: Vec<String>,
    pub(crate) env: Vec<(OsString, OsString)>,
    pub(crate) permits: Arc<Semaphore>,
}

/// Why a [`GuestTool`] printed nothing usable.
#[derive(Debug, Clone, Copy, Error)]
pub(crate) enum GuestToolError {
    #[error("no slot to run the tool in")]
    NoSlot,
    #[error("the tool could not be resolved")]
    Unresolved,
    #[error("the tool could not be started")]
    NotStarted,
    #[error("the tool failed")]
    Failed,
    #[error("the tool timed out")]
    TimedOut,
    #[error("the tool's output was larger than accepted")]
    OutputTooLarge,
}

/// The standard output of `tool` when it exits with 0 within
/// `config.mcp_listing_timeout`, the wait for a slot included.
pub(crate) async fn run(config: &DaemonConfig, tool: GuestTool) -> Result<Vec<u8>, GuestToolError> {
    let deadline = Instant::now() + config.mcp_listing_timeout;
    let Ok(Ok(_slot)) = tokio::time::timeout_at(deadline, tool.permits.acquire()).await else {
        return Err(GuestToolError::NoSlot);
    };
    let resolve_config = config.clone();
    let argv = tool.argv;
    let (spawn_argv, cwd) = tokio::task::spawn_blocking(move || {
        let spawn_argv = crate::profile::resolve_spawn_argv(&resolve_config, &argv)
            .map_err(|_| GuestToolError::Unresolved)?;
        // An empty directory, as a conversion starts in: the guest reads no
        // project's settings from it.
        let cwd: Option<PathBuf> = resolve_config
            .registration_cwd
            .as_ref()
            .filter(|dir| dir.ensure().is_ok())
            .map(|dir| dir.path().to_path_buf());
        Ok::<_, GuestToolError>((spawn_argv, cwd))
    })
    .await
    .map_err(|_| GuestToolError::Unresolved)??;
    let captured = pty::spawn_one_shot(
        spawn_argv,
        cwd,
        pty::CaptureChildEnv::Based {
            base: config.child_base_env.clone(),
            overlay: tool.env,
        },
        None,
    )
    .await
    .map_err(|_| GuestToolError::NotStarted)?;
    let limits = yagura_os::CaptureLimits {
        timeout: deadline.saturating_duration_since(Instant::now()),
        max_output_bytes: config.mcp_registration_max_output_bytes,
    };
    // A timeout ends the tool's whole tree before returning.
    let outcome = tokio::task::spawn_blocking(move || captured.collect(limits))
        .await
        .unwrap_or(yagura_os::CaptureOutcome::TimedOut);
    match outcome {
        yagura_os::CaptureOutcome::Exited {
            code: Some(0),
            stdout,
            ..
        } => Ok(stdout),
        yagura_os::CaptureOutcome::Exited { .. } => Err(GuestToolError::Failed),
        yagura_os::CaptureOutcome::TimedOut => Err(GuestToolError::TimedOut),
        yagura_os::CaptureOutcome::OutputTooLarge => Err(GuestToolError::OutputTooLarge),
    }
}

/// The path `stdout` holds as its one line, when that line names an absolute
/// path on this host and no control character.
pub(crate) fn printed_absolute_path(stdout: &[u8]) -> Option<PathBuf> {
    let text = std::str::from_utf8(stdout).ok()?;
    let line = text.strip_suffix('\n').unwrap_or(text);
    let line = line.strip_suffix('\r').unwrap_or(line);
    let usable = !line.chars().any(char::is_control) && Path::new(line).is_absolute();
    usable.then(|| PathBuf::from(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_line_naming_an_absolute_path_and_no_control_character_is_a_path() {
        let absolute = if cfg!(windows) {
            r"\\wsl.localhost\Ubuntu\home\u"
        } else {
            "/home/u"
        };
        for ending in ["", "\n", "\r\n"] {
            assert_eq!(
                printed_absolute_path(format!("{absolute}{ending}").as_bytes()),
                Some(PathBuf::from(absolute)),
                "a line ended with {ending:?}"
            );
        }
        for (what, printed) in [
            ("nothing", String::new()),
            ("a relative path", "home/u\n".to_string()),
            ("two lines", format!("{absolute}\n{absolute}\n")),
            ("a control character", format!("{absolute}\u{1}\n")),
            (
                "a carriage return inside",
                format!("{absolute}\r{absolute}\n"),
            ),
        ] {
            assert_eq!(
                printed_absolute_path(printed.as_bytes()),
                None,
                "{what} is no path"
            );
        }
        assert_eq!(
            printed_absolute_path(b"\xff/home\n"),
            None,
            "non-UTF-8 is no path"
        );
    }
}
