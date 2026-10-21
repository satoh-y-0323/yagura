//! A profile's top-level `[host_paths]` table: how a daemon-side path is
//! turned into the path the profile's CLI sees, for a CLI that runs in
//! another environment (a WSL distribution started from Windows, say).
//!
//! The table is one fact about the whole profile, so both the paths a launch
//! injects and the paths a registration writes go through the one converter
//! it declares. Without the table, paths are handed over unchanged.

use serde::Deserialize;
use thiserror::Error;

use crate::profile;

/// The placeholder [`HostPaths::convert`] must spell: the daemon-side path.
const PATH_PLACEHOLDER: &str = "{path}";

/// A validated `[host_paths]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPaths {
    convert: Vec<String>,
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
}

/// A `[host_paths]` table before validation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawHostPaths {
    convert: Vec<String>,
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
    Ok(Some(HostPaths {
        convert: raw.convert,
    }))
}
