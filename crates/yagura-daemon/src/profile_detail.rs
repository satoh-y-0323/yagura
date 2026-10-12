//! Every value a profile uses, with the layer it comes from: what the GUI's
//! profile details show. A value is a key whose value is not a table; an
//! array, including an array of tables, is one value.

use serde_json::{Map, Number, Value as Json};
use thiserror::Error;
use toml_edit::{Document, Item, Table, Value};
use yagura_protocol::{ProfileCommandDetail, ProfileDetailEntry};

use crate::config::DaemonConfig;
use crate::profile::{self, Profile};
use crate::profile_layers::{normalized_document, overlay_document};

/// A profile's values as [`read_profile_detail`] reports them.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileDetail {
    /// Whether the profile is laid over a shipped profile.
    pub shipped_base: bool,
    /// The argv in use on the daemon's host.
    pub command: ProfileCommandDetail,
    /// Every value in use, in the profile's own key order.
    pub entries: Vec<ProfileDetailEntry>,
}

/// Why a profile's details could not be read. The text never quotes the
/// file.
#[derive(Debug, Error)]
pub enum ProfileDetailError {
    /// No listed profile has this id.
    #[error("unknown profile")]
    UnknownProfile,
}

/// Reads every value `profile_id` uses, each with the shipped profile's value
/// at the same key and whether the user's own file sets it. Blocking: reads
/// the profiles directory.
///
/// # Errors
///
/// [`ProfileDetailError::UnknownProfile`] wherever
/// [`profile::list_profiles`] would not report the profile.
pub fn read_profile_detail(
    config: &DaemonConfig,
    profile_id: &str,
) -> Result<ProfileDetail, ProfileDetailError> {
    let listed = profile::read_listed_profile(config, profile_id)
        .ok_or(ProfileDetailError::UnknownProfile)?;
    // The listed profile already parsed from these same texts, so none of
    // them can fail to parse here.
    let merged = overlay_document(listed.shipped.unwrap_or(""), &listed.source)
        .map_err(|_| ProfileDetailError::UnknownProfile)?;
    let shipped = listed
        .shipped
        .map(normalized_document)
        .transpose()
        .map_err(|_| ProfileDetailError::UnknownProfile)?;
    let user =
        normalized_document(&listed.source).map_err(|_| ProfileDetailError::UnknownProfile)?;

    let mut entries = Vec::new();
    collect_entries(merged.as_table(), &mut Vec::new(), &mut |path, item| {
        let shipped_value = shipped
            .as_ref()
            .and_then(|shipped| lookup(shipped, path))
            .map(item_to_json);
        entries.push(ProfileDetailEntry {
            path: path.to_vec(),
            value: item_to_json(item),
            shipped: shipped_value,
            changed: listed.shipped.is_none() || user_sets(user.as_table(), path),
        });
    });

    Ok(ProfileDetail {
        shipped_base: listed.shipped.is_some(),
        command: command_detail(
            config,
            &listed.profile,
            listed.shipped,
            user.contains_key("command"),
        ),
        entries,
    })
}

/// The argv `profile` launches on the host, beside the shipped profile's.
pub(crate) fn command_detail(
    config: &DaemonConfig,
    profile: &Profile,
    shipped: Option<&str>,
    user_has_command: bool,
) -> ProfileCommandDetail {
    let shipped_argv = shipped
        .and_then(|source| profile::parse_profile(&profile.id, source).ok())
        .map(|shipped| profile::listed_argv(&shipped, config));
    ProfileCommandDetail {
        argv: profile::listed_argv(profile, config),
        changed: shipped_argv.is_none() || user_has_command,
        shipped: shipped_argv,
    }
}

/// Calls `visit` with the path and item of every key in `table` whose value
/// is not a table, depth first in key order.
fn collect_entries(table: &Table, path: &mut Vec<String>, visit: &mut dyn FnMut(&[String], &Item)) {
    for (key, item) in table.iter() {
        path.push(key.to_string());
        match item {
            Item::Table(child) => collect_entries(child, path, visit),
            Item::None => {}
            other => visit(path, other),
        }
        path.pop();
    }
}

/// The item at `path` in `document`, when every step but the last is a
/// table.
fn lookup<'a>(document: &'a Document, path: &[String]) -> Option<&'a Item> {
    let (last, parents) = path.split_last()?;
    let mut table = document.as_table();
    for key in parents {
        table = table.get(key)?.as_table()?;
    }
    table.get(last)
}

/// Whether `user` sets the value at `path`: it has the key itself, or a
/// value (not a table) on the way to it, which replaces everything below.
fn user_sets(user: &Table, path: &[String]) -> bool {
    let mut table = user;
    for (index, key) in path.iter().enumerate() {
        match table.get(key) {
            None => return false,
            Some(Item::Table(child)) if index + 1 < path.len() => table = child,
            Some(_) => return true,
        }
    }
    false
}

fn item_to_json(item: &Item) -> Json {
    match item {
        Item::Value(value) => value_to_json(value),
        Item::Table(table) => table_to_json(table),
        Item::ArrayOfTables(array) => Json::Array(array.iter().map(table_to_json).collect()),
        Item::None => Json::Null,
    }
}

fn table_to_json(table: &Table) -> Json {
    Json::Object(
        table
            .iter()
            .map(|(key, item)| (key.to_string(), item_to_json(item)))
            .collect::<Map<_, _>>(),
    )
}

fn value_to_json(value: &Value) -> Json {
    match value {
        Value::String(text) => Json::String(text.value().clone()),
        Value::Integer(number) => Json::Number(Number::from(*number.value())),
        Value::Float(number) => Number::from_f64(*number.value()).map_or(Json::Null, Json::Number),
        Value::Boolean(flag) => Json::Bool(*flag.value()),
        Value::Datetime(datetime) => Json::String(datetime.value().to_string()),
        Value::Array(array) => Json::Array(array.iter().map(value_to_json).collect()),
        Value::InlineTable(table) => Json::Object(
            table
                .iter()
                .map(|(key, value)| (key.to_string(), value_to_json(value)))
                .collect::<Map<_, _>>(),
        ),
    }
}
