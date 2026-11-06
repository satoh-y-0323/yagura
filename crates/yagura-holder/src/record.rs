//! The holder's record in its `holders` directory, replaced in one rename so
//! a reader never sees a partial file.

use std::io;
use std::path::{Path, PathBuf};

use yagura_protocol::holder::{HolderRecord, encode_holder_record, holder_record_path};

use crate::endpoint::Place;

fn file_name(path: &Path) -> io::Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))
}

/// The start of the process `pid` as a record keeps it; `None` when it
/// cannot be read, which leaves the process to be told by its id alone.
pub(crate) fn start_of(pid: u32) -> Option<String> {
    match yagura_os::process_start(pid) {
        Ok(start) => start.map(|start| start.to_string()),
        Err(error) => {
            crate::log(format_args!(
                "cannot read the start of process {pid}: {error}"
            ));
            None
        }
    }
}

/// The holder's own record, from the moment the holder owns its endpoint.
/// The record is removed only through this type, on every way the holder
/// ends: by [`RecordFile::remove`], or on drop, an unwind of the holder's
/// core included.
pub(crate) struct RecordFile {
    holders_dir: PathBuf,
    record: HolderRecord,
    removed: bool,
}

/// Proof that the record is removed, which giving up the endpoint asks for:
/// the endpoint goes last, so whoever finds it free finds no record left.
pub(crate) struct RecordRemoved(());

impl RecordFile {
    pub(crate) fn new(place: &Place, record: HolderRecord) -> Self {
        Self {
            holders_dir: place.holders_dir.clone(),
            record,
            removed: false,
        }
    }

    /// Writes the record as it is now.
    pub(crate) fn write(&self) -> io::Result<()> {
        let path = holder_record_path(&self.holders_dir, &self.record.window_id)?;
        let content = encode_holder_record(&self.record);
        // The directory was checked private when the holder started, so it is
        // both the base and the leaf here: nothing above it is touched.
        yagura_os::replace_config_file(
            &self.holders_dir,
            &self.holders_dir,
            &file_name(&path)?,
            content,
        )
    }

    /// Names the started child and its start in the record.
    pub(crate) fn set_child(&mut self, pid: u32) -> io::Result<()> {
        self.record.child_pid = Some(pid);
        self.record.child_start = start_of(pid);
        self.write()
    }

    /// Removes the record now.
    pub(crate) fn remove(&mut self) -> RecordRemoved {
        if !self.removed {
            self.removed = true;
            if let Ok(path) = holder_record_path(&self.holders_dir, &self.record.window_id) {
                let _ = std::fs::remove_file(path);
            }
        }
        RecordRemoved(())
    }
}

impl Drop for RecordFile {
    fn drop(&mut self) {
        self.remove();
    }
}
