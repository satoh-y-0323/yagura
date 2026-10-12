//! The parts every in-place rewrite of a placed config file shares: parsing a
//! source string as an editable document, restoring the file's own line
//! endings, resolving a path to an absolute one, and replacing the file
//! atomically.
//!
//! Each operation built on these keeps its own public error type and converts
//! [`RewriteError`] into it, so the parts carry only the failures they can
//! cause themselves. The log target name (`"profile"` / `"playbook"`) is
//! supplied by the caller, so a single implementation serves both kinds of
//! file.

use std::path::{Path, PathBuf};

use toml_edit::Document;

/// Why a shared rewrite step failed.
#[derive(Debug)]
pub(crate) enum RewriteError {
    /// The file could not be edited or read back as requested.
    RewriteFailed,
    /// Replacing the file failed.
    Io(std::io::Error),
}

pub(crate) fn parse_document(kind: &str, source: &str) -> Result<Document, RewriteError> {
    source.parse::<Document>().map_err(|err| {
        tracing::warn!("failed to parse a {kind} for rewriting: {err}");
        RewriteError::RewriteFailed
    })
}

/// `rendered` with the line endings of `original`: the document writer emits
/// LF only, so a file whose first line ends in CRLF gets CRLF throughout.
pub(crate) fn with_line_endings_of(original: &str, rendered: &str) -> String {
    let uses_crlf = original
        .find('\n')
        .is_some_and(|at| original[..at].ends_with('\r'));
    if uses_crlf {
        rendered.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        rendered.to_string()
    }
}

/// Resolves `path` to an absolute one without reading, creating or otherwise
/// touching anything at that path, and without changing the process's
/// current directory. A relative path is resolved against the process's
/// current directory, the same rule [`replace_file`] relies on so that a bare
/// relative file name never yields an empty parent directory to split.
///
/// # Errors
///
/// Returns any [`std::io::Error`] `std::path::absolute` itself returns (for
/// example, the current directory could not be read while resolving a
/// relative path).
pub(crate) fn absolute_path(path: &Path) -> std::io::Result<PathBuf> {
    std::path::absolute(path)
}

/// Atomically replaces `path` with `content`, the directory split the same
/// way the roles file's save splits it. `path` is resolved with
/// [`absolute_path`] first, so a relative path never yields an empty parent
/// directory to harden.
pub(crate) fn replace_file(kind: &str, path: &Path, content: &str) -> Result<(), RewriteError> {
    let path = absolute_path(path).map_err(RewriteError::Io)?;
    let leaf_dir = path.parent().ok_or(RewriteError::RewriteFailed)?;
    let base_dir = leaf_dir.parent().unwrap_or(leaf_dir);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(RewriteError::RewriteFailed)?;
    yagura_os::replace_config_file(base_dir, leaf_dir, file_name, content).map_err(|err| {
        tracing::warn!("failed to replace {kind} file {}: {err}", path.display());
        RewriteError::Io(err)
    })
}
