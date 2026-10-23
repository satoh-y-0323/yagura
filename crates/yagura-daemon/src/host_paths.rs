//! A profile's top-level `[host_paths]` table: how the daemon reaches the
//! environment the profile's CLI runs in, for a CLI that runs in another
//! environment (a WSL distribution started from Windows, say).
//!
//! The table is one fact about the whole profile, so both the paths a launch
//! injects and the paths a registration writes go through the one converter
//! it declares, and every read or write of the CLI's own settings files goes
//! through the one pair of file tools it declares. Without the table, paths
//! are handed over unchanged and files are read on the host.

use serde::Deserialize;
use thiserror::Error;

use crate::profile;

/// The placeholder [`HostPaths::convert`] must spell: the daemon-side path.
const PATH_PLACEHOLDER: &str = "{path}";
/// The placeholder a file tool's argv may spell: the settings file's path,
/// relative to the CLI environment's home directory.
const FILE_PLACEHOLDER: &str = "{file}";

/// A validated `[host_paths]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPaths {
    convert: Vec<String>,
    files: Option<FileTools>,
}

impl HostPaths {
    /// The command that prints `path` as the CLI's environment sees it: the
    /// table's `convert` argv with `{path}` replaced by `path`.
    #[must_use]
    pub fn render(&self, path: &str) -> Vec<String> {
        self.convert
            .iter()
            .map(|arg| arg.replace(PATH_PLACEHOLDER, path))
            .collect()
    }

    /// The tools that read and replace a settings file in the CLI's
    /// environment, `None` when the table declares no `read`.
    #[must_use]
    pub fn file_tools(&self) -> Option<&FileTools> {
        self.files.as_ref()
    }
}

/// The commands that read and replace one of the CLI's settings files in its
/// own environment: `read` prints the file, `replace` takes its new content on
/// standard input, and `absent_exit` is the exit code `read` uses for "the
/// file is not there".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTools {
    read: Vec<String>,
    replace: Option<Vec<String>>,
    absent_exit: Option<i32>,
}

impl FileTools {
    /// Validates the three keys both `[host_paths]` and
    /// `[mcp_registration.files]` spell: `read` and `replace` may name only
    /// `{file}`.
    pub(crate) fn new(
        read: Vec<String>,
        replace: Option<Vec<String>>,
        absent_exit: Option<i32>,
    ) -> Result<Self, HostPathsError> {
        let names_other = |argv: &[String]| {
            argv.iter()
                .any(|arg| profile::contains_placeholder_outside(arg, &[FILE_PLACEHOLDER]))
        };
        if names_other(&read) || replace.as_deref().is_some_and(names_other) {
            return Err(HostPathsError::UnknownFilePlaceholder);
        }
        Ok(Self {
            read,
            replace,
            absent_exit,
        })
    }

    /// The command that prints `file`'s content.
    #[must_use]
    pub fn render_read(&self, file: &str) -> Vec<String> {
        substitute_file(&self.read, file)
    }

    /// The command that replaces `file`'s content, `None` when there is no
    /// `replace` tool.
    #[must_use]
    pub fn render_replace(&self, file: &str) -> Option<Vec<String>> {
        self.replace
            .as_deref()
            .map(|argv| substitute_file(argv, file))
    }

    /// The exit code `read` uses for "the file is not there", when declared.
    #[must_use]
    pub fn absent_exit(&self) -> Option<i32> {
        self.absent_exit
    }
}

fn substitute_file(argv: &[String], file: &str) -> Vec<String> {
    argv.iter()
        .map(|arg| arg.replace(FILE_PLACEHOLDER, file))
        .collect()
}

/// Why a `[host_paths]` table was refused. Carried only as the `source()` of
/// [`profile::ProfileParseError::InvalidHostPaths`].
#[derive(Debug, Clone, Error)]
pub enum HostPathsError {
    /// `convert` is empty, or has an empty or NUL-holding element.
    #[error("`convert` must be a non-empty argv")]
    InvalidArgv,
    /// No element of `convert` spells `{path}`.
    #[error("`convert` does not name `{{path}}`")]
    MissingPath,
    /// An element of `convert` spells a placeholder other than `{path}`.
    #[error("`convert` names an unknown placeholder")]
    UnknownPlaceholder,
    /// An element of `read` or `replace` spells a placeholder other than
    /// `{file}`.
    #[error("a file tool names an unknown placeholder")]
    UnknownFilePlaceholder,
    /// `replace` or `absent_exit` is declared without the `read` tool.
    #[error("`replace` and `absent_exit` need `read`")]
    FileToolsWithoutRead,
}

/// A `[host_paths]` table before validation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawHostPaths {
    convert: Vec<String>,
    #[serde(default)]
    read: Option<Vec<String>>,
    #[serde(default)]
    replace: Option<Vec<String>>,
    #[serde(default)]
    absent_exit: Option<i32>,
}

/// Validates `raw` into a [`HostPaths`] (`None` in, `None` out).
pub(crate) fn parse_host_paths(
    raw: Option<RawHostPaths>,
) -> Result<Option<HostPaths>, HostPathsError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.convert.is_empty() || yagura_protocol::validate_argv_elements(&raw.convert).is_err() {
        return Err(HostPathsError::InvalidArgv);
    }
    if raw
        .convert
        .iter()
        .any(|arg| profile::contains_placeholder_outside(arg, &[PATH_PLACEHOLDER]))
    {
        return Err(HostPathsError::UnknownPlaceholder);
    }
    if !raw.convert.iter().any(|arg| arg.contains(PATH_PLACEHOLDER)) {
        return Err(HostPathsError::MissingPath);
    }
    let files = match raw.read {
        Some(read) => Some(FileTools::new(read, raw.replace, raw.absent_exit)?),
        None if raw.replace.is_some() || raw.absent_exit.is_some() => {
            return Err(HostPathsError::FileToolsWithoutRead);
        }
        None => None,
    };
    Ok(Some(HostPaths {
        convert: raw.convert,
        files,
    }))
}
