//! Editing named entries of a user's own JSON settings file, in place:
//! [`set_entries`] adds or replaces them, [`remove_entries`] takes them out,
//! and [`entries_hold`] reads back whether they are in place. A key starting
//! with `/` is a JSON Pointer (RFC 6901) through nested objects; any other key
//! names a top-level entry, the same as a one-token pointer. Every other entry
//! keeps its bytes -- content, order and formatting -- because the rewrite
//! splices the document's text rather than serializing a parsed map (whose key
//! order is not the file's). Nothing here reads or writes a file itself.

use serde_json::{Map, Value};
use thiserror::Error;

use crate::config_rewrite;

/// Why a JSON entry rewrite could not be made. Nothing is written in either
/// case: an edit that cannot be made safely leaves the document as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum JsonEntryError {
    /// The document does not parse as JSON.
    #[error("could not be parsed")]
    Unparsable,
    /// The document parses, but is not a JSON object.
    #[error("the document is not an object")]
    NotAnObject,
    /// A pointer's parent exists but holds something other than an object.
    #[error("a value that is not an object is in the way of the entry")]
    ParentNotAnObject,
    /// A pointer holds a `~` not followed by `0` or `1`.
    #[error("the pointer has an invalid escape")]
    InvalidPointer,
}

/// Sets each `(key, value)` of `entries` as an entry of the JSON object
/// `source`, changing nothing else. A pointer's missing parents are created as
/// objects. An entry already present has its value replaced where it stands (a
/// repeated name keeps only its first occurrence, and a pointer passes through
/// the first occurrence of each parent); a new one is added after the last
/// entry of its object, indented like it. `source`'s line endings (LF or CRLF,
/// taken from its first line) are kept.
///
/// # Errors
///
/// Returns [`JsonEntryError::Unparsable`] when `source` is not JSON,
/// [`JsonEntryError::NotAnObject`] when it is JSON but not an object,
/// [`JsonEntryError::ParentNotAnObject`] when a pointer's parent holds a
/// non-object, and [`JsonEntryError::InvalidPointer`] for a bad `~` escape.
pub fn set_entries(source: &str, entries: &[(String, Value)]) -> Result<String, JsonEntryError> {
    let mut text = source.to_string();
    for (key, value) in entries {
        text = set_at(&text, &path_of(key)?, value)?;
    }
    Ok(config_rewrite::with_line_endings_of(source, &text))
}

/// Takes every entry named by a key of `names` out of the JSON object
/// `source`, changing nothing else. A key that is not there, or whose parent
/// is not an object, is skipped; an object the removal empties stays.
///
/// # Errors
///
/// As [`set_entries`], except that a non-object parent is not an error.
pub fn remove_entries(source: &str, names: &[String]) -> Result<String, JsonEntryError> {
    let mut text = source.to_string();
    outline(&text)?;
    for key in names {
        let path = path_of(key)?;
        while let Some(updated) = remove_first(&text, &path)? {
            text = updated;
        }
    }
    Ok(text)
}

/// Whether every `(key, value)` of `entries` is an entry of `source` holding
/// exactly `value`.
///
/// # Errors
///
/// As [`set_entries`], except that a non-object parent only means `false`.
pub fn entries_hold(source: &str, entries: &[(String, Value)]) -> Result<bool, JsonEntryError> {
    let document: Value = serde_json::from_str(source).map_err(|_| JsonEntryError::Unparsable)?;
    if !document.is_object() {
        return Err(JsonEntryError::NotAnObject);
    }
    for (key, value) in entries {
        let found = path_of(key)?
            .iter()
            .try_fold(&document, |node, name| node.as_object()?.get(name));
        if found != Some(value) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether any key of `names` is an entry of `source`, whatever it holds.
///
/// # Errors
///
/// As [`entries_hold`].
pub fn any_present(source: &str, names: &[String]) -> Result<bool, JsonEntryError> {
    let document: Value = serde_json::from_str(source).map_err(|_| JsonEntryError::Unparsable)?;
    if !document.is_object() {
        return Err(JsonEntryError::NotAnObject);
    }
    for key in names {
        let found = path_of(key)?
            .iter()
            .try_fold(&document, |node, name| node.as_object()?.get(name));
        if found.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The names a key walks through: a pointer's unescaped tokens, or the key
/// itself as a single top-level name.
fn path_of(key: &str) -> Result<Vec<String>, JsonEntryError> {
    let Some(pointer) = key.strip_prefix('/') else {
        return Ok(vec![key.to_string()]);
    };
    pointer.split('/').map(unescape_token).collect()
}

fn unescape_token(token: &str) -> Result<String, JsonEntryError> {
    let mut name = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c != '~' {
            name.push(c);
            continue;
        }
        match chars.next() {
            Some('0') => name.push('~'),
            Some('1') => name.push('/'),
            _ => return Err(JsonEntryError::InvalidPointer),
        }
    }
    Ok(name)
}

/// One entry's byte offsets in the document's text.
struct Member {
    key: String,
    start: usize,
    value_start: usize,
    value_end: usize,
}

/// An object's braces and entries, in the order they are written.
struct Outline {
    open: usize,
    close: usize,
    members: Vec<Member>,
}

/// How an entry is laid out next to its neighbours.
enum Layout {
    /// One entry per line, each starting with this indentation.
    Lines(String),
    /// Entries share the line.
    Inline,
}

/// The top-level object of `source`, after checking the whole text is JSON.
fn outline(source: &str) -> Result<Outline, JsonEntryError> {
    let document: Value = serde_json::from_str(source).map_err(|_| JsonEntryError::Unparsable)?;
    if !document.is_object() {
        return Err(JsonEntryError::NotAnObject);
    }
    object_at(source, 0)
}

fn object_at(text: &str, at: usize) -> Result<Outline, JsonEntryError> {
    scan_object(text, at).ok_or(JsonEntryError::Unparsable)
}

/// The object `member` holds.
fn nested_object(text: &str, member: &Member) -> Result<Outline, JsonEntryError> {
    if text.as_bytes().get(member.value_start) != Some(&b'{') {
        return Err(JsonEntryError::ParentNotAnObject);
    }
    object_at(text, member.value_start)
}

fn set_at(text: &str, path: &[String], value: &Value) -> Result<String, JsonEntryError> {
    let Some((name, parents)) = path.split_last() else {
        return Ok(text.to_string());
    };
    let mut object = outline(text)?;
    for (depth, parent) in parents.iter().enumerate() {
        let Some(member) = object.members.iter().find(|member| member.key == *parent) else {
            // The rest of the path does not exist yet: it goes in as one value.
            let below = path.get(depth.saturating_add(1)..).unwrap_or_default();
            let nested = below.iter().rev().fold(value.clone(), |inner, key| {
                Value::Object(Map::from_iter([(key.clone(), inner)]))
            });
            return add_member(text, &object, parent, &nested);
        };
        object = nested_object(text, member)?;
    }
    let text = remove_duplicates(text, object.open, name)?;
    let object = object_at(&text, object.open)?;
    match object.members.iter().position(|member| member.key == *name) {
        Some(index) => {
            let rendered = render_value(value, &layout_of(&text, &object, index))?;
            Ok(object.members.get(index).map_or(text.clone(), |member| {
                splice(&text, member.value_start, member.value_end, &rendered)
            }))
        }
        None => add_member(&text, &object, name, value),
    }
}

/// `text` with `name : value` added as the last entry of `object`.
fn add_member(
    text: &str,
    object: &Outline,
    name: &str,
    value: &Value,
) -> Result<String, JsonEntryError> {
    let key = serde_json::to_string(name).map_err(|_| JsonEntryError::Unparsable)?;
    match object.members.len().checked_sub(1) {
        Some(last_index) => {
            let layout = layout_of(text, object, last_index);
            let rendered = render_value(value, &layout)?;
            let inserted = match &layout {
                Layout::Lines(indent) => format!(",\n{indent}{key}: {rendered}"),
                Layout::Inline => format!(", {key}: {rendered}"),
            };
            let at = object
                .members
                .get(last_index)
                .map_or(object.close, |member| member.value_end);
            Ok(splice(text, at, at, &inserted))
        }
        None => {
            let base = line_indent(text, object.open);
            let indent = format!("{base}  ");
            let rendered = render_value(value, &Layout::Lines(indent.clone()))?;
            let inserted = format!("\n{indent}{key}: {rendered}\n{base}");
            Ok(splice(
                text,
                object.open.saturating_add(1),
                object.close,
                &inserted,
            ))
        }
    }
}

/// The spaces and tabs that start the line holding `at`.
fn line_indent(text: &str, at: usize) -> &str {
    let line_start = text
        .get(..at)
        .and_then(|before| before.rfind('\n'))
        .map_or(0, |newline| newline.saturating_add(1));
    let line = text.get(line_start..).unwrap_or("");
    let width = line
        .len()
        .saturating_sub(line.trim_start_matches([' ', '\t']).len());
    line.get(..width).unwrap_or("")
}

/// `text` with every occurrence of `name` after the first taken out of the
/// object opening at `open`.
fn remove_duplicates(text: &str, open: usize, name: &str) -> Result<String, JsonEntryError> {
    let mut text = text.to_string();
    loop {
        let object = object_at(&text, open)?;
        let mut matching = object
            .members
            .iter()
            .enumerate()
            .filter(|(_, member)| member.key == name)
            .map(|(index, _)| index);
        let (Some(_), Some(second)) = (matching.next(), matching.next()) else {
            return Ok(text);
        };
        text = remove_member(&text, &object, second);
    }
}

/// `text` with the first entry at `path` taken out, `None` when there is none.
fn remove_first(text: &str, path: &[String]) -> Result<Option<String>, JsonEntryError> {
    let Some((name, parents)) = path.split_last() else {
        return Ok(None);
    };
    let mut object = outline(text)?;
    for parent in parents {
        let Some(member) = object.members.iter().find(|member| member.key == *parent) else {
            return Ok(None);
        };
        object = match nested_object(text, member) {
            Ok(nested) => nested,
            Err(JsonEntryError::ParentNotAnObject) => return Ok(None),
            Err(err) => return Err(err),
        };
    }
    Ok(object
        .members
        .iter()
        .position(|member| member.key == *name)
        .map(|index| remove_member(text, &object, index)))
}

/// Takes the entry at `index` out together with the separator that joins it
/// to a neighbour, so the remaining entries keep their own layout.
fn remove_member(text: &str, outline: &Outline, index: usize) -> String {
    let members = &outline.members;
    let Some(member) = members.get(index) else {
        return text.to_string();
    };
    if let Some(next) = members.get(index.saturating_add(1)) {
        return splice(text, member.start, next.start, "");
    }
    if let Some(previous) = index.checked_sub(1).and_then(|at| members.get(at)) {
        return splice(text, previous.value_end, member.value_end, "");
    }
    splice(text, outline.open.saturating_add(1), outline.close, "")
}

fn splice(text: &str, from: usize, to: usize, with: &str) -> String {
    let (Some(head), Some(tail)) = (text.get(..from), text.get(to..)) else {
        return text.to_string();
    };
    format!("{head}{with}{tail}")
}

/// The layout of the entry at `index`: on a line of its own when only
/// whitespace precedes it since the last line break.
fn layout_of(text: &str, outline: &Outline, index: usize) -> Layout {
    let Some(member) = outline.members.get(index) else {
        return Layout::Inline;
    };
    let Some(before) = text.get(..member.start) else {
        return Layout::Inline;
    };
    match before.rfind('\n') {
        Some(line_start) => {
            let indent = before.get(line_start.saturating_add(1)..).unwrap_or("");
            if indent.chars().all(|c| c == ' ' || c == '\t') {
                Layout::Lines(indent.to_string())
            } else {
                Layout::Inline
            }
        }
        None => Layout::Inline,
    }
}

fn render_value(value: &Value, layout: &Layout) -> Result<String, JsonEntryError> {
    match layout {
        Layout::Lines(indent) => serde_json::to_string_pretty(value)
            .map(|pretty| pretty.replace('\n', &format!("\n{indent}")))
            .map_err(|_| JsonEntryError::Unparsable),
        Layout::Inline => serde_json::to_string(value).map_err(|_| JsonEntryError::Unparsable),
    }
}

// ---------------------------------------------------------------------
// Scanning the text of a document already known to be a JSON object
// ---------------------------------------------------------------------

fn scan_object(source: &str, from: usize) -> Option<Outline> {
    let bytes = source.as_bytes();
    let open = skip_whitespace(bytes, from);
    if bytes.get(open) != Some(&b'{') {
        return None;
    }
    let mut members = Vec::new();
    let mut at = skip_whitespace(bytes, open.saturating_add(1));
    if bytes.get(at) == Some(&b'}') {
        return Some(Outline {
            open,
            close: at,
            members,
        });
    }
    loop {
        let start = at;
        let key_end = string_end(bytes, start)?;
        let key: String = serde_json::from_str(source.get(start..key_end)?).ok()?;
        at = skip_whitespace(bytes, key_end);
        if bytes.get(at) != Some(&b':') {
            return None;
        }
        let value_start = skip_whitespace(bytes, at.saturating_add(1));
        let value_end = value_end(bytes, value_start)?;
        members.push(Member {
            key,
            start,
            value_start,
            value_end,
        });
        at = skip_whitespace(bytes, value_end);
        match bytes.get(at)? {
            b',' => at = skip_whitespace(bytes, at.saturating_add(1)),
            b'}' => {
                return Some(Outline {
                    open,
                    close: at,
                    members,
                });
            }
            _ => return None,
        }
    }
}

fn skip_whitespace(bytes: &[u8], mut at: usize) -> usize {
    while bytes
        .get(at)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
    {
        at = at.saturating_add(1);
    }
    at
}

/// The offset just past the string starting at `at`.
fn string_end(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes.get(at) != Some(&b'"') {
        return None;
    }
    let mut cursor = at.saturating_add(1);
    loop {
        match bytes.get(cursor)? {
            b'\\' => cursor = cursor.saturating_add(2),
            b'"' => return Some(cursor.saturating_add(1)),
            _ => cursor = cursor.saturating_add(1),
        }
    }
}

/// The offset just past the value starting at `at`.
fn value_end(bytes: &[u8], at: usize) -> Option<usize> {
    match bytes.get(at)? {
        b'"' => string_end(bytes, at),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut cursor = at;
            loop {
                match bytes.get(cursor)? {
                    b'"' => {
                        cursor = string_end(bytes, cursor)?;
                        continue;
                    }
                    b'{' | b'[' => depth = depth.saturating_add(1),
                    b'}' | b']' => {
                        depth = depth.checked_sub(1)?;
                        if depth == 0 {
                            return Some(cursor.saturating_add(1));
                        }
                    }
                    _ => {}
                }
                cursor = cursor.saturating_add(1);
            }
        }
        _ => {
            let mut cursor = at;
            while bytes.get(cursor).is_some_and(|byte| {
                !matches!(byte, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
            }) {
                cursor = cursor.saturating_add(1);
            }
            Some(cursor)
        }
    }
}
