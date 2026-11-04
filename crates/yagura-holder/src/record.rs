//! The files a holder writes in its `holders` directory, each replaced in one
//! rename so a reader never sees a partial file.

use std::io;

use yagura_protocol::holder::{
    HolderExit, HolderRecord, encode_holder_exit, encode_holder_record, holder_exit_path,
    holder_record_path,
};

use crate::endpoint::Place;

fn file_name(path: &std::path::Path) -> io::Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))
}

/// Publishes `record` as the holder's record.
pub(crate) fn write_record(place: &Place, record: &HolderRecord) -> io::Result<()> {
    let path = holder_record_path(&place.holders_dir, &record.window_id)?;
    replace_in_holders_dir(place, &path, encode_holder_record(record))
}

/// Removes the holder's record.
pub(crate) fn remove_record(place: &Place, window_id: &str) {
    if let Ok(path) = holder_record_path(&place.holders_dir, window_id) {
        let _ = std::fs::remove_file(path);
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

/// Writes the child's exit and the output kept with it.
pub(crate) fn write_exit(place: &Place, window_id: &str, exit: &HolderExit) -> io::Result<()> {
    let path = holder_exit_path(&place.holders_dir, window_id)?;
    replace_in_holders_dir(place, &path, encode_holder_exit(exit))
}

/// The directory itself was checked private when the holder started, so it is
/// both the base and the leaf here: nothing above it is touched.
fn replace_in_holders_dir(
    place: &Place,
    path: &std::path::Path,
    content: Vec<u8>,
) -> io::Result<()> {
    yagura_os::replace_config_file(
        &place.holders_dir,
        &place.holders_dir,
        &file_name(path)?,
        content,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use yagura_protocol::holder::read_holder_exit;
    use yagura_test_support::paths::TempDir;

    // Output over one frame's size is written as several frames; reading must
    // give back the same bytes, so a wrong position in a later frame fails here.
    #[test]
    fn an_exit_with_more_output_than_one_frame_reads_back_unchanged() {
        let dir = TempDir::new("holder-record", "big-exit");
        let holders_dir = dir.path().join("holders");
        let place = Place {
            holders_dir: holders_dir.clone(),
            endpoint: String::new(),
            daemon_endpoint: String::new(),
            user_id: String::new(),
        };
        let len = 2 * 1024 * 1024 + 12_345;
        let output: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let exit = HolderExit {
            code: Some(7),
            checkpoint: None,
            output,
            end_seq: len as u64,
        };

        write_exit(&place, "w1", &exit).unwrap();

        assert_eq!(read_holder_exit(&holders_dir, "w1").unwrap(), exit);
    }
}
