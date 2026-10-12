//! The bundled pseudoconsole every Windows window runs under.
//!
//! The ConPTY built into Windows answers a child's terminal queries itself and
//! repaints its own screen after a resize, which makes it a second terminal
//! next to the daemon's. The bundled one (`conpty.dll` and `OpenConsole.exe`,
//! `vendor/conpty`) leaves both to the daemon. The library falls back to the
//! built-in host silently when `OpenConsole.exe` is not beside it, so the pair
//! is checked before a window opens; the DLL itself is then loaded by its full
//! path from the same directory (`pty::spawn`), never by a search.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::bundled_conpty_pins::{self, PinFault};

/// The bundled pseudoconsole is missing or altered.
#[derive(Debug, thiserror::Error)]
pub enum BundledConptyError {
    /// A file of the pair is absent, unreadable, or not the pinned build.
    #[error("bundled ConPTY files are not intact: {0}")]
    NotIntact(#[from] PinFault),
    /// The running executable's own location could not be determined.
    #[error("cannot locate the running executable: {0}")]
    NoExecutable(std::io::Error),
}

/// Checks that `dir` holds `conpty.dll` and `OpenConsole.exe` with the pinned
/// contents.
///
/// # Errors
///
/// Returns [`BundledConptyError::NotIntact`] when either file is absent,
/// unreadable, or hashes differently from the pinned build.
pub fn verify_beside(dir: &Path) -> Result<(), BundledConptyError> {
    bundled_conpty_pins::check_dir(dir)?;
    Ok(())
}

pub(crate) fn executable_dir() -> Result<PathBuf, BundledConptyError> {
    let exe = std::env::current_exe().map_err(BundledConptyError::NoExecutable)?;
    exe.parent().map(Path::to_path_buf).ok_or_else(|| {
        BundledConptyError::NoExecutable(std::io::Error::other(
            "executable has no parent directory",
        ))
    })
}

/// Checks the pair beside the running executable. A pass is remembered for the
/// life of the process; a failure is checked again on the next call. Does
/// nothing off Windows, where windows have no pseudoconsole host to bundle.
///
/// # Errors
///
/// As [`verify_beside`], plus [`BundledConptyError::NoExecutable`].
pub fn verify_beside_executable() -> Result<(), BundledConptyError> {
    if !cfg!(windows) {
        return Ok(());
    }
    static VERIFIED: OnceLock<()> = OnceLock::new();
    if VERIFIED.get().is_some() {
        return Ok(());
    }
    verify_beside(&executable_dir()?)?;
    let _ = VERIFIED.set(());
    Ok(())
}
