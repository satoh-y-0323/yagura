//! Switching a profile on or off by rewriting the top-level `enabled` key of
//! its user file, the file that carries the user's changes over the shipped
//! profile.
//!
//! Profile files are written by hand, so the switch edits the parsed document:
//! switching off replaces the key's value or adds `enabled = false` (creating
//! the file when there is none), switching on removes the key, and every other
//! byte stays as the operator wrote it.

use toml_edit::{Item, Value};

use crate::config::DaemonConfig;
use crate::config_rewrite;
use crate::profile_rewrite::{ProfileWriteError, read_placed};

/// The top-level key a profile is switched with.
const ENABLED_KEY: &str = "enabled";

/// The outcome of [`set_profile_enabled`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnabledSet {
    /// Whether the profile is enabled after the call.
    pub enabled: bool,
    /// Whether the file was rewritten (`false` when it was already in the
    /// requested state).
    pub written: bool,
}

/// Why a profile was not switched. Switching refuses no request of its
/// own: every failure is the profile's or the file's.
pub type ProfileEnabledError = ProfileWriteError<std::convert::Infallible>;

/// Switches `profile_id` on or off. Switching off sets the top-level
/// `enabled` key to `false`, keeping the old value's surrounding whitespace
/// and same-line comment, or adds `enabled = false` ahead of the first table
/// when the key is absent; switching on removes the key together with any
/// comment lines directly above it. A request matching the profile's current
/// state writes nothing. Blocking: reads and replaces a file.
///
/// # Errors
///
/// [`ProfileEnabledError::UnknownProfile`] when the profile is not listed
/// (a switched-off profile is still listed),
/// [`ProfileEnabledError::RewriteFailed`] when no profiles directory is
/// configured or the edited document, laid over the shipped profile, does not
/// read back with the requested state and the same model aliases and choices,
/// and [`ProfileEnabledError::Io`] when the file cannot be replaced. On every
/// error the file is left as it was.
pub fn set_profile_enabled(
    config: &DaemonConfig,
    profile_id: &str,
    enabled: bool,
) -> Result<EnabledSet, ProfileEnabledError> {
    let placed = read_placed(config, profile_id)?;
    let listed = &placed.listed;
    if listed.profile.enabled == enabled {
        return Ok(EnabledSet {
            enabled,
            written: false,
        });
    }

    let mut document = config_rewrite::parse_document("profile", &listed.source)?;
    let root = document.as_table_mut();
    if enabled {
        root.remove(ENABLED_KEY);
    } else if let Some(value) = root.get_mut(ENABLED_KEY).and_then(Item::as_value_mut) {
        let mut replacement = Value::from(false);
        *replacement.decor_mut() = value.decor().clone();
        *value = replacement;
    } else {
        root.insert(ENABLED_KEY, toml_edit::value(false));
    }

    let rewritten = config_rewrite::with_line_endings_of(&listed.source, &document.to_string());
    let before = &listed.profile.options;
    let read_back_matches = listed.reread_with(&rewritten).is_some_and(|profile| {
        profile.enabled == enabled
            && profile.options.model.as_ref().map(|model| &model.aliases)
                == before.model.as_ref().map(|model| &model.aliases)
            && profile.options.model_choices == before.model_choices
    });
    if !read_back_matches {
        return Err(ProfileEnabledError::RewriteFailed);
    }
    config_rewrite::replace_file("profile", &placed.path, &rewritten)?;
    Ok(EnabledSet {
        enabled,
        written: true,
    })
}
