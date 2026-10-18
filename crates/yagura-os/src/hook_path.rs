//! Paths a CLI hook command can start with.
//!
//! Some CLIs hand a hook command to a shell and escape any quotes yagura
//! writes around a path, so a path holding whitespace cannot be written into
//! a hook command at all; this module finds a whitespace-free form of it.

use std::path::Path;

/// Why no whitespace-free form of a path could be found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SpacelessPathError {
    /// The path is not valid Unicode, so it cannot be written into a text
    /// settings file.
    #[error("the daemon's path is not valid Unicode, so a hook command cannot name it")]
    NotUnicode,
    /// The path holds whitespace, and this platform has no other name for it.
    #[error(
        "the daemon's path holds whitespace, and a CLI cannot run a hook command whose path \
         holds whitespace"
    )]
    Whitespace,
    /// The path holds whitespace, and it has no 8.3 short name free of it.
    #[error(
        "the daemon's path holds whitespace, which a CLI cannot run in a hook command, and it \
         has no 8.3 short name without whitespace (short names may be turned off on its volume)"
    )]
    NoShortName,
}

/// Pure choice between a path and its short name: `long` when it holds no
/// whitespace, otherwise `short` when it is given and holds none. `None` when
/// neither can be written into a hook command.
#[must_use]
pub fn spaceless_path_from(long: &str, short: Option<&str>) -> Option<String> {
    let spaceless = |path: &&str| !path.is_empty() && !path.contains(char::is_whitespace);
    Some(long)
        .filter(spaceless)
        .or_else(|| short.filter(spaceless))
        .map(str::to_string)
}

/// `path` in a form a hook command can start with: unchanged when it holds
/// no whitespace; on Windows, otherwise its 8.3 short name
/// (`GetShortPathNameW`), which needs `path` to exist.
///
/// # Errors
///
/// [`SpacelessPathError::NotUnicode`] when `path` is not valid Unicode;
/// [`SpacelessPathError::NoShortName`] on Windows when it holds whitespace
/// and no short name free of whitespace can be read;
/// [`SpacelessPathError::Whitespace`] elsewhere when it holds whitespace.
pub fn spaceless_path(path: &Path) -> Result<String, SpacelessPathError> {
    let long = path.to_str().ok_or(SpacelessPathError::NotUnicode)?;
    if let Some(path) = spaceless_path_from(long, None) {
        return Ok(path);
    }
    #[cfg(windows)]
    {
        spaceless_path_from(long, short_path_name(path).as_deref())
            .ok_or(SpacelessPathError::NoShortName)
    }
    #[cfg(not(windows))]
    {
        Err(SpacelessPathError::Whitespace)
    }
}

#[cfg(windows)]
fn short_path_name(path: &Path) -> Option<String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows::Win32::Storage::FileSystem::GetShortPathNameW;
    use windows::core::PCWSTR;

    let mut long: Vec<u16> = path.as_os_str().encode_wide().collect();
    if long.contains(&0) {
        return None;
    }
    long.push(0);
    // SAFETY: `long` is NUL-terminated and outlives the call; a missing buffer
    // only asks for the size, in characters including the terminating NUL.
    let needed = unsafe { GetShortPathNameW(PCWSTR(long.as_ptr()), None) };
    if needed == 0 {
        return None;
    }
    let mut short = vec![0u16; needed as usize];
    // SAFETY: `long` is NUL-terminated, and `short` is live for the call with
    // the length the wrapper passes.
    let written = unsafe { GetShortPathNameW(PCWSTR(long.as_ptr()), Some(&mut short)) };
    // A result not below the buffer length is the size it would now need.
    if written == 0 || written as usize >= short.len() {
        return None;
    }
    OsString::from_wide(&short[..written as usize])
        .into_string()
        .ok()
}
