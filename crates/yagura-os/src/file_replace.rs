//! Atomic replacement of a file that must already exist, for writing back a
//! configuration file another program owns.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Distinguishes temporary files made by concurrent calls in one process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomically replaces the content of the existing file at `path` with
/// `content`: the bytes are written to a temporary file next to the real
/// file and renamed onto it, so a reader never observes a partial write.
///
/// Unlike [`crate::replace_config_file`], this never creates the target,
/// never secures (or even touches) the parent directory, and never narrows
/// the file's permissions: the file belongs to another program and may hold
/// that program's other settings. A symlink is followed and the real file is
/// replaced, leaving the link itself in place. On Unix the real file's mode
/// is carried over to the replacement; its owner is not (the replacement is
/// owned by the calling user).
///
/// # Errors
///
/// Returns an [`std::io::Error`] when `path` does not exist (nothing is
/// created), when it does not resolve to a regular file, or when creating,
/// writing or renaming the temporary file fails -- e.g. an unwritable
/// directory, or a target the platform refuses to replace (a read-only file
/// on Windows). The temporary file is removed on any failure after it was
/// created, and the original content is left as it was.
pub fn replace_existing_file(path: &Path, content: &str) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path)?;
    let metadata = std::fs::metadata(&target)?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the target is not a regular file",
        ));
    }
    let temp_path = temp_path_beside(&target)?;

    // `create_new` refuses to reuse (or follow) anything already at the
    // temporary path.
    let mut temp = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    let written = temp
        .write_all(content.as_bytes())
        .and_then(|()| temp.sync_all())
        .and_then(|()| copy_mode(&temp_path, &metadata));
    drop(temp);
    let result = written.and_then(|()| std::fs::rename(&temp_path, &target));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

/// A hidden, not-yet-existing sibling path of `target` for the temporary
/// file.
fn temp_path_beside(target: &Path) -> std::io::Result<PathBuf> {
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the target has no parent directory or file name",
        ));
    };
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    Ok(dir.join(temp_name))
}

/// Gives the temporary file the real file's mode before the rename, so the
/// replacement is neither wider nor narrower than what it replaces.
#[cfg(unix)]
fn copy_mode(temp_path: &Path, original: &std::fs::Metadata) -> std::io::Result<()> {
    std::fs::set_permissions(temp_path, original.permissions())
}

/// Windows keeps only a read-only flag here, and copying it would make the
/// temporary file impossible to remove after a failed rename; a read-only
/// target already refuses the rename itself.
#[cfg(not(unix))]
fn copy_mode(_temp_path: &Path, _original: &std::fs::Metadata) -> std::io::Result<()> {
    Ok(())
}
