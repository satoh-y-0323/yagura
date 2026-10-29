//! Editing tables of a user's own TOML settings file, in place: [`apply`] and
//! [`apply_edits`] set keys, [`remove_tables`] deletes tables, and
//! [`edits_hold`] reads back whether a set of edits is in place. Each rewrite
//! parses the document and hands back its text, touching nothing but the
//! named keys and tables. Nothing here reads or writes a file itself -- that
//! is [`crate::mcp_registration`]'s `toml_set` operation, layered on top with
//! the file's own path.

use thiserror::Error;
use toml_edit::{Item, TableLike, Value};

use crate::config_rewrite;

/// Why a `toml_set` rewrite could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TomlSetError {
    /// `source` does not parse as TOML.
    #[error("could not be parsed")]
    Unparsable,
    /// A table an edit does not create does not exist in `source`. Nothing is
    /// written: a table that is not there means the file being edited is not
    /// the one the CLI reads.
    #[error("the table was not found")]
    TableNotFound,
    /// A table an edit creates is blocked by a key of the same name that
    /// holds something other than a table. Nothing is written: the user's
    /// own value is never replaced.
    #[error("a value is in the way of the table")]
    NotATable,
}

/// One table [`apply_edits`] sets keys in.
#[derive(Debug, Clone)]
pub struct TableEdit<'a> {
    /// The table's path: each segment one literal key, never split on `.`.
    pub table: Vec<String>,
    /// The keys and values to set in it.
    pub values: &'a toml::Table,
    /// Whether the table (and any table on its path) is created when it is
    /// not there. `false` requires it to exist already.
    pub create: bool,
}

/// Sets each key in `values` under the existing table `table` names, inside
/// `source`, changing nothing else: no other table, key, comment, ordering
/// or line ending moves. A key already present keeps its own surrounding
/// whitespace and same-line comment; a key that is not there yet is added as
/// one new line, at the end of the table's own keys. Keys of `table`'s
/// existing table that are not named in `values` are left exactly as they
/// were. `table`'s own line endings (LF or CRLF, taken from its first line)
/// are preserved in the result.
///
/// # Errors
///
/// Returns [`TomlSetError::Unparsable`] when `source` does not parse as
/// TOML, and [`TomlSetError::TableNotFound`] when `table` names a path that
/// does not exist in it -- in neither case is anything about `source`
/// reflected in the error.
pub fn apply(source: &str, table: &[String], values: &toml::Table) -> Result<String, TomlSetError> {
    apply_edits(
        source,
        &[TableEdit {
            table: table.to_vec(),
            values,
            create: false,
        }],
    )
}

/// Makes every edit in `edits` inside `source`, the way [`apply`] makes one,
/// or none of them. An edit with [`TableEdit::create`] adds the tables its
/// path is missing: the ones along the way without a header of their own,
/// the table itself as one new `[...]` section placed after the tables that
/// precede it in the document.
///
/// # Errors
///
/// Returns [`TomlSetError::Unparsable`] when `source` does not parse,
/// [`TomlSetError::TableNotFound`] when an edit that does not create names a
/// missing table, and [`TomlSetError::NotATable`] when a key on a created
/// table's path holds a value that is not a table.
pub fn apply_edits(source: &str, edits: &[TableEdit<'_>]) -> Result<String, TomlSetError> {
    let mut document = config_rewrite::parse_document("mcp registration", source)
        .map_err(|_| TomlSetError::Unparsable)?;
    for edit in edits {
        let target = table_at_mut(document.as_table_mut(), &edit.table, edit.create)?;
        for (key, value) in edit.values {
            set_value(target, key, to_edit_value(value));
        }
    }
    Ok(config_rewrite::with_line_endings_of(
        source,
        &document.to_string(),
    ))
}

/// Deletes each table in `tables` from `source`, changing nothing else. A
/// table that is not there (or whose path runs through a value that is not a
/// table) is already gone and is skipped.
///
/// # Errors
///
/// Returns [`TomlSetError::Unparsable`] when `source` does not parse.
pub fn remove_tables(source: &str, tables: &[Vec<String>]) -> Result<String, TomlSetError> {
    let mut document = config_rewrite::parse_document("mcp registration", source)
        .map_err(|_| TomlSetError::Unparsable)?;
    for path in tables {
        let Some((leaf, parent)) = path.split_last() else {
            continue;
        };
        if let Ok(parent) = table_at_mut(document.as_table_mut(), parent, false) {
            parent.remove(leaf);
        }
    }
    Ok(config_rewrite::with_line_endings_of(
        source,
        &document.to_string(),
    ))
}

/// Whether every key of every edit in `edits` is present in `source` with
/// exactly the value the edit sets.
///
/// # Errors
///
/// Returns [`TomlSetError::Unparsable`] when `source` does not parse.
pub fn edits_hold(source: &str, edits: &[TableEdit<'_>]) -> Result<bool, TomlSetError> {
    let document: toml::Table = source.parse().map_err(|_| TomlSetError::Unparsable)?;
    Ok(edits.iter().all(|edit| {
        let table = edit
            .table
            .iter()
            .try_fold(&document, |table, segment| table.get(segment)?.as_table());
        table.is_some_and(|table| {
            edit.values
                .iter()
                .all(|(key, value)| table.get(key) == Some(value))
        })
    }))
}

/// Whether `source` holds a table at `table` (each segment one literal key).
///
/// # Errors
///
/// Returns [`TomlSetError::Unparsable`] when `source` does not parse.
pub fn table_exists(source: &str, table: &[String]) -> Result<bool, TomlSetError> {
    let document: toml::Table = source.parse().map_err(|_| TomlSetError::Unparsable)?;
    Ok(table
        .iter()
        .try_fold(&document, |table, segment| table.get(segment)?.as_table())
        .is_some())
}

/// Walks `path` through nested tables starting at `root`: each segment is
/// looked up as one literal key (never split on `.`), so a quoted key holding
/// a literal dot (`"a.b"`) is a different table than two nested keys
/// (`a.b`). A missing segment is created when `create` holds -- without a
/// header of its own unless it is the last one -- and is
/// [`TomlSetError::TableNotFound`] otherwise.
fn table_at_mut<'a>(
    root: &'a mut dyn TableLike,
    path: &[String],
    create: bool,
) -> Result<&'a mut dyn TableLike, TomlSetError> {
    let blocked = if create {
        TomlSetError::NotATable
    } else {
        TomlSetError::TableNotFound
    };
    let mut current = root;
    for (index, segment) in path.iter().enumerate() {
        if current.get(segment).is_none() {
            if !create {
                return Err(TomlSetError::TableNotFound);
            }
            let mut table = toml_edit::Table::new();
            table.set_implicit(index + 1 < path.len());
            current.insert(segment, Item::Table(table));
        }
        current = current
            .get_mut(segment)
            .and_then(Item::as_table_like_mut)
            .ok_or(blocked)?;
    }
    Ok(current)
}

/// Sets `key` to `value` on `table`: an existing value keeps its own decor
/// (leading whitespace and any same-line comment), a new key is inserted at
/// the end of the table's own keys.
fn set_value(table: &mut dyn TableLike, key: &str, value: Value) {
    if let Some(existing) = table.get_mut(key).and_then(Item::as_value_mut) {
        let mut replacement = value;
        *replacement.decor_mut() = existing.decor().clone();
        *existing = replacement;
    } else {
        table.insert(key, Item::Value(value));
    }
}

/// Converts a value read from a profile's own TOML (`toml::Value`) into the
/// editable value type ([`toml_edit::Value`]) [`apply`] writes with.
fn to_edit_value(value: &toml::Value) -> Value {
    match value {
        toml::Value::String(text) => Value::from(text.as_str()),
        toml::Value::Integer(n) => Value::from(*n),
        toml::Value::Float(n) => Value::from(*n),
        toml::Value::Boolean(b) => Value::from(*b),
        toml::Value::Datetime(datetime) => Value::from(*datetime),
        toml::Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(to_edit_value(item));
            }
            Value::from(array)
        }
        toml::Value::Table(entries) => {
            let mut inline = toml_edit::InlineTable::new();
            for (key, item) in entries {
                inline.insert(key, to_edit_value(item));
            }
            Value::from(inline)
        }
    }
}
