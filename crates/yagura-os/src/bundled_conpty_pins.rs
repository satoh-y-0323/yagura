//! The pinned identity of the bundled pseudoconsole (`vendor/conpty/x64`), and
//! the one check of a directory against it.
//!
//! This file is the only place the hashes are written. `yagura-os` uses it for
//! the run-time check and the daemon's build script includes it by path, so the
//! build-time and run-time checks cannot drift apart. It depends on `std` and
//! `sha2` only, for that reason.

use std::fmt;
use std::path::Path;

use sha2::{Digest, Sha256};

/// One file of the bundled pseudoconsole and the SHA-256 it must have.
#[derive(Debug, Clone, Copy)]
pub struct PinnedFile {
    /// File name inside the directory.
    pub name: &'static str,
    /// Lower-case hex SHA-256 of the file's contents.
    pub sha256: &'static str,
}

/// `Microsoft.Windows.Console.ConPTY` 1.25.260930003, x64. The same values are
/// listed in `vendor/conpty/README.md` and checked by `scripts/dep-audit`.
pub const PINNED_FILES: [PinnedFile; 2] = [
    PinnedFile {
        name: "conpty.dll",
        sha256: "feeef341d891643c62d30b6b07800bc70f0bc148f44cb8c3bee84aa557ae805a",
    },
    PinnedFile {
        name: "OpenConsole.exe",
        sha256: "3d66b23d0a71bb8eed2b77edc8b9df9bf54ce6c8fb74c863a30e760997f80586",
    },
];

/// Why a directory is not the bundled pseudoconsole.
#[derive(Debug)]
pub enum PinFault {
    /// A pinned file could not be read (absent or inaccessible).
    Unreadable {
        /// The pinned file's name.
        name: &'static str,
        /// The underlying read error.
        source: std::io::Error,
    },
    /// A pinned file's contents hash to something else.
    Mismatch {
        /// The pinned file's name.
        name: &'static str,
        /// The hash the file actually has.
        actual: String,
    },
}

impl fmt::Display for PinFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { name, source } => write!(f, "{name} cannot be read: {source}"),
            Self::Mismatch { name, actual } => {
                write!(f, "{name} is not the pinned file (sha256 {actual})")
            }
        }
    }
}

impl std::error::Error for PinFault {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unreadable { source, .. } => Some(source),
            Self::Mismatch { .. } => None,
        }
    }
}

/// Lower-case hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Checks that `dir` holds every pinned file with its pinned contents.
///
/// # Errors
///
/// Returns the first [`PinFault`] found, in [`PINNED_FILES`] order.
pub fn check_dir(dir: &Path) -> Result<(), PinFault> {
    for pinned in &PINNED_FILES {
        let bytes =
            std::fs::read(dir.join(pinned.name)).map_err(|source| PinFault::Unreadable {
                name: pinned.name,
                source,
            })?;
        let actual = sha256_hex(&bytes);
        if actual != pinned.sha256 {
            return Err(PinFault::Mismatch {
                name: pinned.name,
                actual,
            });
        }
    }
    Ok(())
}
