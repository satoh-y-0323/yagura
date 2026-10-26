//! Reading an operator's config file with a size cap.

use std::io::Read;
use std::path::Path;

/// The largest config file read, in bytes. A config file is a short table an
/// operator writes; the cap keeps a stray large file from being loaded whole
/// on every request that reads it.
pub const MAX_CONFIG_FILE_BYTES: usize = 64 * 1024;

/// Why [`read_config_file`] returned no bytes.
#[derive(Debug)]
pub(crate) enum ConfigFileError {
    /// The file exists but could not be read.
    Io(std::io::Error),
    /// The file is larger than [`MAX_CONFIG_FILE_BYTES`].
    TooLarge,
}

/// Reads `path` whole, or `None` when it does not exist. Reads at most one
/// byte past the cap, so an oversized file is rejected without being loaded.
pub(crate) fn read_config_file(path: &Path) -> Result<Option<Vec<u8>>, ConfigFileError> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(ConfigFileError::Io(err)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(ConfigFileError::Io)?;
    if bytes.len() > MAX_CONFIG_FILE_BYTES {
        return Err(ConfigFileError::TooLarge);
    }
    Ok(Some(bytes))
}
