pub mod approval;
pub mod chat_store;
pub mod child_env;
pub mod cli;
pub mod config;
mod config_rewrite;
pub mod control;
pub mod detector;
pub mod handoff;
pub mod health;
pub mod host_paths;
pub mod inbox;
pub mod input;
pub mod input_encode;
pub mod ipc;
pub mod json_entry;
pub mod live_policy;
pub mod mcp_registration;
pub mod model_choices;
pub mod palette_store;
pub mod plan;
pub mod plan_permit;
pub mod plan_store;
pub mod playbook_rewrite;
pub mod profile;
pub mod profile_aliases;
pub mod profile_command;
pub mod profile_delivery;
pub mod profile_detail;
pub mod profile_enabled;
mod profile_layers;
pub mod profile_rewrite;
pub mod pty;
pub mod roles;
pub mod room;
mod sql;
pub mod store;
pub mod terminal_identity;
pub mod ticket;
pub mod token;
pub mod toml_set;
pub mod waiter;
pub mod window;
pub mod workspace;

use std::path::{Path, PathBuf};
use thiserror::Error;

pub use config::DaemonConfig;
pub use ipc::{IpcError, IpcServer};
pub use pty::{PtyError, PtySession};
pub use window::{OpenWindowRequest, RequestedCwd, WindowEntry, WindowOpenError, WindowRegistry};
pub use yagura_protocol;

/// Error type for shell resolution.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ShellResolveError {
    #[error("no shell found")]
    NotFound,
}

/// Relative path from a Program Files-style folder to PowerShell 7's
/// executable.
const PWSH_RELATIVE_PATH: [&str; 3] = ["PowerShell", "7", "pwsh.exe"];

/// Relative path from `%SystemRoot%` to the Windows PowerShell 5.1
/// executable.
const SYSTEM_POWERSHELL_RELATIVE_PATH: [&str; 4] =
    ["System32", "WindowsPowerShell", "v1.0", "powershell.exe"];

/// Where [`windows_shell_candidate`] looks for a default Windows shell, and
/// how it decides a candidate can be started.
///
/// Every field is data (the same pattern [`yagura_os::program::Platform`]
/// uses), so the search runs -- and is tested -- on every host operating
/// system rather than only under `#[cfg(windows)]`. [`WindowsShellSearch::from_env`]
/// builds one from the current process's own environment.
#[derive(Debug, Clone)]
pub struct WindowsShellSearch {
    /// Program Files-style folders searched, in order, for `PowerShell\7\pwsh.exe`
    /// (typically `%ProgramFiles%` then `%ProgramFiles(x86)%`).
    pub program_files: Vec<PathBuf>,
    /// `%SystemRoot%`, under which Windows PowerShell 5.1 is looked for.
    /// `None` means no such folder may be assumed.
    pub system_root: Option<PathBuf>,
    /// The `PATH` (and, on Windows, npm shim directory) lookup `pwsh` /
    /// `powershell` fall back to when neither `program_files` nor
    /// `system_root` has them.
    pub lookup: yagura_os::program::ProgramLookup,
    /// Decides whether a candidate path can be started. A plain `fn`, not a
    /// closure, so it stays `Clone` / `Debug` and cheap to pass around.
    pub is_executable: fn(&Path) -> bool,
}

impl WindowsShellSearch {
    /// Builds a search from the current process's own environment:
    /// `program_files` from `%ProgramFiles%` then `%ProgramFiles(x86)%` (a
    /// variable that is unset is simply skipped, never assumed), `system_root`
    /// from `%SystemRoot%`, and `lookup` from
    /// [`yagura_os::program::ProgramLookup::from_env`].
    #[must_use]
    pub fn from_env() -> Self {
        let program_files = ["ProgramFiles", "ProgramFiles(x86)"]
            .into_iter()
            .filter_map(|name| std::env::var_os(name).map(PathBuf::from))
            .collect();
        Self {
            program_files,
            system_root: std::env::var_os("SystemRoot").map(PathBuf::from),
            lookup: yagura_os::program::ProgramLookup::from_env(),
            is_executable: yagura_os::program::default_is_executable,
        }
    }
}

/// Finds the default Windows shell under `search`'s rules, without assuming
/// any drive letter or installation folder that `search` did not name.
///
/// Order: `pwsh.exe` under each of `search.program_files` in turn, then
/// `pwsh` looked up the way the operating system would look up a bare
/// `PATH` name ([`yagura_os::program::resolve_program`] under
/// [`yagura_os::program::Platform::Windows`], accepting only
/// [`yagura_os::program::Resolution::Direct`] -- a `.cmd` / `.bat` shim on
/// `PATH` is never picked as a default shell), then `powershell.exe` under
/// `search.system_root`, then `powershell` looked up the same way on `PATH`.
/// `None` when nothing along that chain is executable.
///
/// Compiles and runs on every host operating system ([`Platform`] is passed
/// as a value, not read from a `#[cfg]`), so it can be exercised without a
/// Windows host.
///
/// [`Platform`]: yagura_os::program::Platform
#[must_use]
pub fn windows_shell_candidate(search: &WindowsShellSearch) -> Option<PathBuf> {
    search
        .program_files
        .iter()
        .map(|dir| join_all(dir, &PWSH_RELATIVE_PATH))
        .find(|candidate| (search.is_executable)(candidate))
        .or_else(|| windows_path_candidate("pwsh", search))
        .or_else(|| {
            let candidate = join_all(
                search.system_root.as_ref()?,
                &SYSTEM_POWERSHELL_RELATIVE_PATH,
            );
            (search.is_executable)(&candidate).then_some(candidate)
        })
        .or_else(|| windows_path_candidate("powershell", search))
}

/// Joins `base` with every element of `parts`, in order.
fn join_all(base: &Path, parts: &[&str]) -> PathBuf {
    parts
        .iter()
        .fold(base.to_path_buf(), |acc, part| acc.join(part))
}

/// Looks `program` (a bare name, no extension) up on `search.lookup` the way
/// the operating system would, accepting only a directly startable result.
fn windows_path_candidate(program: &str, search: &WindowsShellSearch) -> Option<PathBuf> {
    let argv = [program.to_string()];
    match yagura_os::program::resolve_program(
        &argv,
        yagura_os::program::Platform::Windows,
        &search.lookup,
        &search.is_executable,
    ) {
        Ok(yagura_os::program::Resolution::Direct(path)) => Some(path),
        _ => None,
    }
}

/// Resolve the shell executable to use.
///
/// This pure function resolves the shell based on the configured path,
/// `windows_search` (Windows), and the caller's `$SHELL` value (Unix). It
/// reads no process environment variable itself; `shell_env` and
/// `windows_search` are passed in by the caller.
///
/// Windows resolution order:
/// 1. If `configured` is Some and the path exists, canonicalize
///    (normalized to the on-disk true casing) and return it with the
///    extended-length prefix (`\\?\`) stripped; if canonicalization
///    fails, fall through to the [`windows_shell_candidate`] chain (step 2)
///    rather than returning `configured` as-is
/// 2. Fall back to [`windows_shell_candidate`] against `windows_search`
/// 3. If that fails too, return `NotFound`
///
/// Unix resolution order (`windows_search` is never consulted):
/// `configured` -> `shell_env` -> the literal `/bin/sh`.
/// Every step, including the final `/bin/sh` literal, requires the path to
/// exist; a system without a `/bin/sh` returns `NotFound` rather than a
/// dangling path. The configured path is returned as-is, **not
/// canonicalized** (the `\\?\` strip is a Windows-only concept).
///
/// # Arguments
///
/// * `configured` - Optional configured shell path (e.g., from configuration file)
/// * `shell_env` - The caller's `$SHELL` environment value, if any (Unix only)
/// * `windows_search` - Where to look for a default Windows shell (Windows only)
///
/// # Returns
///
/// Ok(PathBuf) pointing to an existing shell executable. Returns
/// ShellResolveError if none found.
pub fn resolve_shell(
    configured: Option<&Path>,
    shell_env: Option<&Path>,
    windows_search: &WindowsShellSearch,
) -> Result<PathBuf, ShellResolveError> {
    // Unix-only input: acknowledged here so Windows builds do not flag it as
    // unused. `Option<&Path>` is `Copy`, so this does not consume it.
    let _ = shell_env;
    // Windows-only input: acknowledged here so Unix builds do not flag it as
    // unused. `&WindowsShellSearch` is `Copy`, so this does not consume it.
    let _ = windows_search;

    if let Some(path) = configured
        && path.exists()
    {
        #[cfg(windows)]
        match path.canonicalize() {
            Ok(canonical) => return Ok(strip_extended_length_prefix(&canonical)),
            Err(_) => {
                // Canonicalization failure falls through to the fallback chain.
            }
        }
        // Unix does not canonicalize: the configured path is returned
        // exactly as given.
        #[cfg(not(windows))]
        return Ok(path.to_path_buf());
    }

    #[cfg(windows)]
    {
        if let Some(found) = windows_shell_candidate(windows_search) {
            if let Ok(canonical) = found.canonicalize() {
                return Ok(strip_extended_length_prefix(&canonical));
            }
            return Ok(found);
        }
    }

    // shell_env -> the /bin/sh literal. /bin/sh is checked for existence like
    // every other candidate, so a system without it reports `NotFound`
    // instead of a dangling path.
    #[cfg(not(windows))]
    {
        if let Some(path) = shell_env
            && path.exists()
        {
            return Ok(path.to_path_buf());
        }
        let sh = PathBuf::from("/bin/sh");
        if sh.exists() {
            return Ok(sh);
        }
        Err(ShellResolveError::NotFound)
    }

    #[cfg(windows)]
    Err(ShellResolveError::NotFound)
}

/// Strips the Windows extended-length prefix (`\\?\`) left by `canonicalize`:
/// the process-creation APIs do not accept that form.
/// `\\?\UNC\server\share` is restored to `\\server\share`.
///
/// `pub(crate)` so [`window`] can reuse it for `cwd` normalization: one
/// implementation of the strip for both call sites.
pub(crate) fn strip_extended_length_prefix(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}
