//! The files a holder writes in its `holders` directory, each replaced in one
//! rename so a reader never sees a partial file.

use std::io;
use std::path::{Path, PathBuf};

use yagura_protocol::holder::{
    HolderExit, HolderRecord, encode_holder_exit, encode_holder_record, holder_exit_path,
    holder_record_path,
};

use crate::endpoint::Place;
use crate::session::ExitKept;

fn file_name(path: &Path) -> io::Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))
}

/// The holder's own record, from the moment the holder owns its endpoint.
/// The record is removed only through this type; when it is to be removed
/// on drop, that also happens on an unwind of the holder's core.
pub(crate) struct RecordFile {
    holders_dir: PathBuf,
    record: HolderRecord,
    remove_on_drop: bool,
    removed: bool,
}

impl RecordFile {
    /// `remove_on_drop` is decided by `Provisional::own_record` (`session.rs`).
    pub(crate) fn new(place: &Place, record: HolderRecord, remove_on_drop: bool) -> Self {
        Self {
            holders_dir: place.holders_dir.clone(),
            record,
            remove_on_drop,
            removed: false,
        }
    }

    pub(crate) fn window_id(&self) -> &str {
        &self.record.window_id
    }

    /// Writes the record as it is now.
    pub(crate) fn write(&self) -> io::Result<()> {
        let path = holder_record_path(&self.holders_dir, &self.record.window_id)?;
        replace_in_holders_dir(&self.holders_dir, &path, encode_holder_record(&self.record))
    }

    /// Names the started child in the record.
    pub(crate) fn set_child_pid(&mut self, pid: u32) -> io::Result<()> {
        self.record.child_pid = Some(pid);
        self.write()
    }

    /// Removes the record now, for a holder that ends without its child
    /// having started.
    pub(crate) fn remove(&mut self) {
        if !self.removed {
            self.removed = true;
            if let Ok(path) = holder_record_path(&self.holders_dir, &self.record.window_id) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

impl Drop for RecordFile {
    fn drop(&mut self) {
        if self.remove_on_drop {
            self.remove();
        }
    }
}

/// Removes an exit file an earlier holder of the same window left, so no
/// reader takes it for this holder's. Called once this holder owns the
/// window's endpoint, which the earlier one let go of only after writing it.
pub(crate) fn remove_stale_exit(place: &Place, window_id: &str) -> io::Result<()> {
    match std::fs::remove_file(holder_exit_path(&place.holders_dir, window_id)?) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Writes the child's exit next to `record`.
/// Only a holder whose exit is kept for a next daemon holds an [`ExitKept`].
pub(crate) fn write_exit(
    record: &RecordFile,
    _kept: ExitKept,
    exit: &HolderExit,
) -> io::Result<()> {
    write_exit_at(&record.holders_dir, record.window_id(), exit)
}

fn write_exit_at(holders_dir: &Path, window_id: &str, exit: &HolderExit) -> io::Result<()> {
    let path = holder_exit_path(holders_dir, window_id)?;
    replace_in_holders_dir(holders_dir, &path, encode_holder_exit(exit))
}

/// The directory itself was checked private when the holder started, so it is
/// both the base and the leaf here: nothing above it is touched.
fn replace_in_holders_dir(holders_dir: &Path, path: &Path, content: Vec<u8>) -> io::Result<()> {
    yagura_os::replace_config_file(holders_dir, holders_dir, &file_name(path)?, content)
}
