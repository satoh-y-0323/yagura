//! Locating a profile's user file for a rewrite: resolving `profile_id` to
//! the file that carries the user's changes over the shipped profile, and the
//! one error every rewrite of it reports. Each rewrite built on top only has
//! to describe the requests it refuses. Parsing, line-ending restoration,
//! path resolution and atomic replacement are shared with the playbook
//! rewrite in `config_rewrite`.

use std::path::PathBuf;

use thiserror::Error;

use crate::config::DaemonConfig;
use crate::config_rewrite;
use crate::profile::{self, ListedProfileFile};

/// Why a profile's user file was not rewritten. `R` is what the one rewrite
/// refuses in a request it reads as wrong. The text of every variant is a
/// fixed phrase or `R`'s own: none carries the file's content or path.
#[derive(Debug, Error)]
pub enum ProfileWriteError<R: std::error::Error + 'static> {
    /// No listed profile has this id (an invalid id, neither a shipped
    /// profile nor a file by that id, a profile the parser rejects, or one
    /// with nothing to launch on the host operating system).
    #[error("unknown profile")]
    UnknownProfile,
    /// The request is one this rewrite refuses.
    #[error(transparent)]
    Refused(R),
    /// No profiles directory is configured, or the edited document did not
    /// read back as requested.
    #[error("the profile file could not be rewritten as requested")]
    RewriteFailed,
    /// Replacing the file failed.
    #[error("the profile file could not be written")]
    Io(#[source] std::io::Error),
}

impl<R: std::error::Error + 'static> From<config_rewrite::RewriteError> for ProfileWriteError<R> {
    fn from(err: config_rewrite::RewriteError) -> Self {
        match err {
            config_rewrite::RewriteError::RewriteFailed => Self::RewriteFailed,
            config_rewrite::RewriteError::Io(err) => Self::Io(err),
        }
    }
}

/// A listed profile and the path its user file is written to, whether or
/// not that file exists yet.
pub(crate) struct PlacedProfile {
    pub(crate) path: PathBuf,
    pub(crate) listed: ListedProfileFile,
}

/// Reads the listed profile `profile_id` names. A profile its `enabled` key
/// switches off is still read: the rewrite that switches it back on needs it.
pub(crate) fn read_placed<R: std::error::Error + 'static>(
    config: &DaemonConfig,
    profile_id: &str,
) -> Result<PlacedProfile, ProfileWriteError<R>> {
    let listed = profile::read_listed_profile(config, profile_id)
        .ok_or(ProfileWriteError::UnknownProfile)?;
    let path = listed
        .path
        .clone()
        .ok_or(ProfileWriteError::RewriteFailed)?;
    Ok(PlacedProfile { path, listed })
}
