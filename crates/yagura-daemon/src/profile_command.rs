//! Setting the argv a profile launches on the daemon's host, or putting the
//! shipped one back, in the profile's user file.
//!
//! The argv is written under this host's own `[command]` key, so it applies
//! here whatever `default` the shipped profile has, and leaves the other
//! operating systems' keys alone. Putting the shipped argv back removes the
//! user file's whole `[command]` table. Every other byte of the file stays as
//! the operator wrote it.

use thiserror::Error;
use toml_edit::{Array, Item, Table, value};
use yagura_protocol::{ArgvElementError, ProfileCommandDetail, validate_argv_elements};

use crate::config::DaemonConfig;
use crate::config_rewrite;
use crate::profile::ProfileOs;
use crate::profile_detail::command_detail;
use crate::profile_rewrite::{ProfileWriteError, read_placed};

/// A command [`set_profile_command`] refuses: an empty argv, or an element
/// that is empty or holds a NUL.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct InvalidCommand(pub String);

/// Why a command was not set.
pub type ProfileCommandError = ProfileWriteError<InvalidCommand>;

/// Sets `profile_id`'s argv on this host to `argv`, or with `None` removes
/// the user file's `[command]` so the shipped argv applies again, and returns
/// the argv in use afterwards. A request matching the file's current state
/// writes nothing. Blocking: reads and replaces a file.
///
/// # Errors
///
/// [`ProfileCommandError::UnknownProfile`] when the profile is not listed,
/// [`ProfileCommandError::Refused`] for an empty argv or an empty or
/// NUL-holding element, [`ProfileCommandError::RewriteFailed`] when no
/// profiles directory is configured or the edited document does not read
/// back with the requested argv, and [`ProfileCommandError::Io`] when the file
/// cannot be replaced. On every error the file is left as it was.
pub fn set_profile_command(
    config: &DaemonConfig,
    profile_id: &str,
    argv: Option<&[String]>,
) -> Result<ProfileCommandDetail, ProfileCommandError> {
    if let Some(argv) = argv {
        validate_command(argv)?;
    }
    let placed = read_placed(config, profile_id)?;
    let listed = &placed.listed;

    let mut document = config_rewrite::parse_document("profile", &listed.source)?;
    let root = document.as_table_mut();
    match argv {
        Some(argv) => {
            let command = root
                .entry("command")
                .or_insert_with(|| Item::Table(Table::new()))
                .as_table_like_mut()
                .ok_or(ProfileCommandError::RewriteFailed)?;
            command.insert(
                host_key(config.host_os),
                value(argv.iter().collect::<Array>()),
            );
        }
        None => {
            root.remove("command");
        }
    }
    let user_has_command = argv.is_some();

    let rewritten = config_rewrite::with_line_endings_of(&listed.source, &document.to_string());
    let profile = listed
        .reread_with(&rewritten)
        .ok_or(ProfileCommandError::RewriteFailed)?;
    let detail = command_detail(config, &profile, listed.shipped, user_has_command);
    let requested = argv.map_or_else(|| detail.shipped.clone(), |argv| Some(argv.to_vec()));
    if requested.as_ref() != Some(&detail.argv) {
        return Err(ProfileCommandError::RewriteFailed);
    }
    if rewritten != listed.source {
        config_rewrite::replace_file("profile", &placed.path, &rewritten)?;
    }
    Ok(detail)
}

/// The `[command]` key that selects an argv on `os`.
fn host_key(os: ProfileOs) -> &'static str {
    match os {
        ProfileOs::Windows => "windows",
        ProfileOs::MacOs => "macos",
        ProfileOs::Linux => "linux",
    }
}

fn validate_command(argv: &[String]) -> Result<(), ProfileCommandError> {
    if argv.is_empty() {
        return Err(ProfileCommandError::Refused(InvalidCommand(
            "the command needs at least one argument".to_string(),
        )));
    }
    validate_argv_elements(argv).map_err(|err: ArgvElementError| {
        ProfileCommandError::Refused(InvalidCommand(err.to_string()))
    })
}
