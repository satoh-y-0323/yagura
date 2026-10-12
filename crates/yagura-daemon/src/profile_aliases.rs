//! Saving a profile's model alias values into its user file, the file that
//! carries the user's changes over the shipped profile.
//!
//! Profile files are written by hand, so the save edits the parsed document
//! rather than re-serialising the profile: every byte outside the changed
//! aliases stays as the operator wrote it. An alias saved with its shipped
//! value is removed from the user file rather than written, so the shipped
//! value keeps reaching the user when a later build changes it. Each rewrite
//! is read back, laid over the shipped profile, before it replaces the file,
//! and nothing is written when that read-back differs from the request.

use std::collections::BTreeMap;

use thiserror::Error;
use toml_edit::{Document, InlineTable, Item, Table, TableLike, Value};
use yagura_protocol::validate_launch_option_value;

use crate::config::DaemonConfig;
use crate::config_rewrite;
use crate::profile::parse_profile;
use crate::profile_rewrite::{ProfileWriteError, read_placed};

/// The outcome of [`save_model_aliases`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedAliases {
    /// Every alias and its value after the save, in the profile's order.
    pub aliases: Vec<(String, String)>,
    /// Whether the file was rewritten (`false` when no value changed).
    pub written: bool,
}

/// An alias save [`save_model_aliases`] refuses. The text of every variant
/// is a fixed phrase, or an alias name and a rule's wording.
#[derive(Debug, Error)]
pub enum AliasRefusal {
    /// The profile has no `[model]` table.
    #[error("the profile has no model option")]
    NoModelOption,
    /// The request's aliases are not exactly the profile's: one is missing,
    /// extra or named twice.
    #[error("the aliases do not match the profile's aliases")]
    AliasSetChanged,
    /// A value breaks the launch option value rule.
    #[error("{alias}: {reason}")]
    InvalidValue {
        /// The alias whose value was refused.
        alias: String,
        /// What the value breaks.
        reason: String,
    },
}

/// Why a profile's aliases were not saved.
pub type AliasSaveError = ProfileWriteError<AliasRefusal>;

/// Replaces the values of `profile_id`'s model aliases with `aliases`. Only
/// the aliases whose value changes touch the user file: one changed back to
/// its shipped value is removed from it, any other is written into its
/// `[model.aliases]` table (keeping the old value's whitespace and same-line
/// comment, and creating the table or the file when there is none). A request
/// changing no value writes nothing. Blocking: reads and replaces a file.
///
/// # Errors
///
/// [`AliasSaveError::UnknownProfile`] when the profile is not listed,
/// [`AliasRefusal::NoModelOption`] when it has no `[model]` table,
/// [`AliasRefusal::AliasSetChanged`] when `aliases` does not name each of
/// the profile's aliases exactly once, [`AliasRefusal::InvalidValue`] for a
/// value breaking the launch option value rule,
/// [`AliasSaveError::RewriteFailed`] when no profiles directory is configured
/// or the edited document does not read back as requested, and
/// [`AliasSaveError::Io`] when the file cannot be replaced. On every error the
/// file is left as it was.
pub fn save_model_aliases(
    config: &DaemonConfig,
    profile_id: &str,
    aliases: &[(String, String)],
) -> Result<SavedAliases, AliasSaveError> {
    let placed = read_placed(config, profile_id)?;
    let listed = &placed.listed;
    let current = listed
        .profile
        .options
        .model
        .as_ref()
        .ok_or(AliasSaveError::Refused(AliasRefusal::NoModelOption))?
        .aliases
        .clone();

    let requested = requested_values(&current, aliases)?;
    for (alias, value) in aliases {
        validate_launch_option_value(value).map_err(|err| {
            AliasSaveError::Refused(AliasRefusal::InvalidValue {
                alias: alias.clone(),
                reason: err.to_string(),
            })
        })?;
    }
    let saved: Vec<(String, String)> = current
        .iter()
        .map(|(alias, old)| {
            let value = requested.get(alias.as_str()).map_or(old, |value| *value);
            (alias.clone(), value.clone())
        })
        .collect();
    if saved == current {
        return Ok(SavedAliases {
            aliases: saved,
            written: false,
        });
    }

    let shipped = shipped_aliases(profile_id, listed.shipped);
    let mut document = config_rewrite::parse_document("profile", &listed.source)?;
    let table = user_aliases_table(&mut document).ok_or(AliasSaveError::RewriteFailed)?;
    for ((alias, old), (_, new)) in current.iter().zip(&saved) {
        if old == new {
            continue;
        }
        if shipped.get(alias.as_str()) == Some(new) {
            table.remove(alias);
        } else {
            set_string_value(table, alias, new);
        }
    }
    prune_empty_model_tables(&mut document);

    let rewritten = config_rewrite::with_line_endings_of(&listed.source, &document.to_string());
    let read_back = listed
        .reread_with(&rewritten)
        .and_then(|profile| profile.options.model)
        .map(|model| model.aliases);
    if read_back.as_ref() != Some(&saved) {
        return Err(AliasSaveError::RewriteFailed);
    }
    config_rewrite::replace_file("profile", &placed.path, &rewritten)?;
    Ok(SavedAliases {
        aliases: saved,
        written: true,
    })
}

/// The requested value of each alias, keyed by alias, when the request names
/// every alias in `current` exactly once and nothing else.
fn requested_values<'a>(
    current: &[(String, String)],
    aliases: &'a [(String, String)],
) -> Result<BTreeMap<&'a str, &'a String>, AliasSaveError> {
    let requested: BTreeMap<&str, &String> = aliases
        .iter()
        .map(|(alias, value)| (alias.as_str(), value))
        .collect();
    let same_set = requested.len() == aliases.len()
        && requested.len() == current.len()
        && current
            .iter()
            .all(|(alias, _)| requested.contains_key(alias.as_str()));
    if same_set {
        Ok(requested)
    } else {
        Err(AliasSaveError::Refused(AliasRefusal::AliasSetChanged))
    }
}

/// The alias values of the shipped profile the user file is laid over.
fn shipped_aliases(profile_id: &str, shipped: Option<&str>) -> BTreeMap<String, String> {
    shipped
        .and_then(|source| parse_profile(profile_id, source).ok())
        .and_then(|profile| profile.options.model)
        .map(|model| model.aliases.into_iter().collect())
        .unwrap_or_default()
}

/// The user document's `model.aliases` table in whichever form it is written
/// (a standard table, an inline table or dotted keys), created when absent.
/// `None` when `model` or `model.aliases` holds something other than a table.
fn user_aliases_table(document: &mut Document) -> Option<&mut dyn TableLike> {
    let model = document
        .entry("model")
        .or_insert_with(|| Item::Table(implicit_table()));
    // A child must take its parent's form: a standard table inserted inside
    // an inline one is never rendered.
    match model {
        Item::Value(Value::InlineTable(inline)) => inline
            .entry("aliases")
            .or_insert_with(|| Value::InlineTable(InlineTable::new()))
            .as_inline_table_mut()
            .map(|table| table as &mut dyn TableLike),
        other => other
            .as_table_like_mut()?
            .entry("aliases")
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_like_mut(),
    }
}

fn implicit_table() -> Table {
    let mut table = Table::new();
    table.set_implicit(true);
    table
}

/// Sets the string value under `alias`, keeping an old value's surrounding
/// whitespace and same-line comment.
fn set_string_value(table: &mut dyn TableLike, alias: &str, new: &str) {
    let mut replacement = Value::from(new);
    if let Some(old) = table.get(alias).and_then(Item::as_value) {
        *replacement.decor_mut() = old.decor().clone();
    }
    table.insert(alias, Item::Value(replacement));
}

/// Removes `model.aliases`, then `model`, when nothing is left in them, so a
/// user file whose every change was reverted carries no empty tables.
fn prune_empty_model_tables(document: &mut Document) {
    let Some(model) = document.get_mut("model").and_then(Item::as_table_like_mut) else {
        return;
    };
    if model
        .get("aliases")
        .and_then(Item::as_table_like)
        .is_some_and(TableLike::is_empty)
    {
        model.remove("aliases");
    }
    if model.is_empty() {
        document.remove("model");
    }
}
