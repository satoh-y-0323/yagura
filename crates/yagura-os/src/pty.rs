//! Starting a program on a pseudoterminal, the same way on every platform.
//!
//! [`spawn`] returns the pieces a window is built from: the byte streams, a
//! handle for resizing and closing the terminal, the child to wait on and to
//! kill, and the [`ProcessGroup`] that owns everything the child starts. The
//! group exists from before the child runs its first instruction, so nothing
//! the child starts can escape it.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::PathBuf;

use crate::ProcessGroup;

#[cfg(not(windows))]
use crate::platform_unix::pty as platform;
#[cfg(windows)]
use crate::platform_windows::conpty as platform;

pub use platform::{PtyChild, PtyControl, PtyKiller};

/// The size of a terminal in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtySize {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
}

/// A change to the environment the child starts with, applied in order.
#[derive(Clone, PartialEq, Eq)]
pub enum EnvOp {
    /// Sets a variable, replacing one of the same name (compared without
    /// regard to case on Windows).
    Set(OsString, OsString),
    /// Removes a variable if it is there.
    Remove(OsString),
}

impl std::fmt::Debug for EnvOp {
    // An environment routinely carries tokens, so neither name nor value is shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Set(..) => f.write_str("EnvOp::Set(..)"),
            Self::Remove(..) => f.write_str("EnvOp::Remove(..)"),
        }
    }
}

/// What to start on the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyCommand {
    /// The program followed by its arguments. On Windows the program is a path
    /// or a name looked up in the child's own `PATH`; a `.cmd` / `.bat` must
    /// already be wrapped (see [`crate::program::into_spawn_argv`]).
    pub argv: Vec<String>,
    /// The working directory. One that is not a directory is replaced by the
    /// user's home directory; `None` means the home directory too.
    pub cwd: Option<PathBuf>,
    /// Changes to the base environment ([`default_env_var`]).
    pub env: Vec<EnvOp>,
}

/// A started child and the terminal it runs on.
///
/// The fields drop in this order: the group first, which ends the child's
/// tree, then the terminal.
pub struct PtySpawn {
    /// Waits for the child.
    pub child: PtyChild,
    /// Kills the child; usable while another thread waits.
    pub killer: PtyKiller,
    /// The child's process tree, which exists from before the child runs.
    pub group: ProcessGroup,
    /// Resizes and closes the terminal.
    pub control: PtyControl,
    /// What the child writes. Read it until it ends: the terminal's host
    /// cannot finish while its output is neither read nor closed.
    pub reader: Box<dyn Read + Send>,
    /// What the child reads.
    pub writer: Box<dyn Write + Send>,
}

impl std::fmt::Debug for PtySpawn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySpawn")
            .field("child", &self.child)
            .finish_non_exhaustive()
    }
}

/// Why [`spawn`] could not start the child.
#[derive(Debug, thiserror::Error)]
pub enum PtySpawnError {
    /// The pseudoconsole bundled beside the executable is missing, altered, or
    /// cannot be loaded.
    #[error("the bundled pseudoconsole is unavailable: {0}")]
    BundledConptyUnavailable(String),
    /// The child could not be started.
    #[error("failed to start the child: {0}")]
    Spawn(#[source] std::io::Error),
}

/// Starts `command` on a new pseudoterminal of `size`.
///
/// The child is inside its [`ProcessGroup`] before it runs any code. The base
/// environment is [`default_env_var`]'s, changed by `command.env`.
///
/// # Errors
///
/// [`PtySpawnError::BundledConptyUnavailable`] on Windows when the bundled
/// pseudoconsole is not intact or cannot be loaded, and
/// [`PtySpawnError::Spawn`] when `argv` is empty or holds a NUL, the program
/// cannot be found or started, or the child cannot be placed in its group. In
/// every error case no child is left running.
pub fn spawn(command: &PtyCommand, size: PtySize) -> Result<PtySpawn, PtySpawnError> {
    platform::spawn(command, size)
}

/// The value `name` has in the environment a child starts from before
/// [`PtyCommand::env`] is applied.
///
/// On Windows that environment includes the machine's and user's persistent
/// variables from the registry, so it can differ from this process's own.
#[must_use]
pub fn default_env_var(name: &OsStr) -> Option<OsString> {
    platform::default_env_var(name)
}
