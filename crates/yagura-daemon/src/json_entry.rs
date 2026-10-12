//! Editing named top-level entries of a user's own JSON settings file, in
//! place: [`set_entries`] adds or replaces them, [`remove_entries`] takes them
//! out, and [`entries_hold`] reads back whether they are in place. Every other
//! entry keeps its bytes -- content, order and formatting -- because the
//! rewrite splices the document's text rather than serializing a parsed map
//! (whose key order is not the file's). Nothing here reads or writes a file
//! itself.

use serde_json::Value;
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
}

/// Sets each `(name, value)` of `entries` as a top-level entry of the JSON
/// object `source`, changing nothing else. An entry already present has its
/// value replaced where it stands (a repeated name keeps only its first
/// occurrence); a new one is added after the last entry, indented like it.
/// `source`'s line endings (LF or CRLF, taken from its first line) are kept.
///
/// # Errors
///
/// Returns [`JsonEntryError::Unparsable`] when `source` is not JSON, and
/// [`JsonEntryError::NotAnObject`] when it is JSON but not an object.
pub fn set_entries(source: &str, entries: &[(String, Value)]) -> Result<String, JsonEntryError> {
    let mut text = source.to_string();
    for (name, value) in entries {
        text = remove_duplicates(&text, name)?;
        text = set_one(&text, name, value)?;
    }
    Ok(config_rewrite::with_line_endings_of(source, &text))
}

/// Takes every top-level entry named in `names` out of the JSON object
/// `source`, changing nothing else. A name that is not there is skipped.
///
/// # Errors
///
/// As [`set_entries`].
pub fn remove_entries(source: &str, names: &[String]) -> Result<String, JsonEntryError> {
    let mut text = source.to_string();
    for name in names {
        while let Some(updated) = remove_first(&text, name)? {
            text = updated;
        }
    }
    outline(&text)?;
    Ok(text)
}

/// Whether every `(name, value)` of `entries` is a top-level entry of
/// `source` holding exactly `value`.
///
/// # Errors
///
/// As [`set_entries`].
pub fn entries_hold(source: &str, entries: &[(String, Value)]) -> Result<bool, JsonEntryError> {
    let document: Value = serde_json::from_str(source).map_err(|_| JsonEntryError::Unparsable)?;
    let object = document.as_object().ok_or(JsonEntryError::NotAnObject)?;
    Ok(entries
        .iter()
        .all(|(name, value)| object.get(name) == Some(value)))
}

/// One top-level entry's byte offsets in the document's text.
struct Member {
    key: String,
    start: usize,
    value_start: usize,
    value_end: usize,
}

/// The top-level object's braces and entries, in the order they are written.
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

fn outline(source: &str) -> Result<Outline, JsonEntryError> {
    let document: Value = serde_json::from_str(source).map_err(|_| JsonEntryError::Unparsable)?;
    if !document.is_object() {
        return Err(JsonEntryError::NotAnObject);
    }
    scan_object(source).ok_or(JsonEntryError::Unparsable)
}

fn set_one(text: &str, name: &str, value: &Value) -> Result<String, JsonEntryError> {
    let outline = outline(text)?;
    if let Some(index) = outline.members.iter().position(|member| member.key == name)
        && let Some(member) = outline.members.get(index)
    {
        let rendered = render_value(value, &layout_of(text, &outline, index))?;
        return Ok(splice(
            text,
            member.value_start,
            member.value_end,
            &rendered,
        ));
    }
    let key = serde_json::to_string(name).map_err(|_| JsonEntryError::Unparsable)?;
    match outline.members.len().checked_sub(1) {
        Some(last_index) => {
            let layout = layout_of(text, &outline, last_index);
            let rendered = render_value(value, &layout)?;
            let inserted = match &layout {
                Layout::Lines(indent) => format!(",\n{indent}{key}: {rendered}"),
                Layout::Inline => format!(", {key}: {rendered}"),
            };
            let at = outline
                .members
                .get(last_index)
                .map_or(outline.close, |member| member.value_end);
            Ok(splice(text, at, at, &inserted))
        }
        None => {
            let indent = "  ";
            let rendered = render_value(value, &Layout::Lines(indent.to_string()))?;
            let inserted = format!("\n{indent}{key}: {rendered}\n");
            Ok(splice(
                text,
                outline.open.saturating_add(1),
                outline.close,
                &inserted,
            ))
        }
    }
}

/// `text` with every occurrence of `name` after the first taken out.
fn remove_duplicates(text: &str, name: &str) -> Result<String, JsonEntryError> {
    let mut text = text.to_string();
    loop {
        let outline = outline(&text)?;
        let mut matching = outline
            .members
            .iter()
            .enumerate()
            .filter(|(_, member)| member.key == name)
            .map(|(index, _)| index);
        let (Some(_), Some(second)) = (matching.next(), matching.next()) else {
            return Ok(text);
        };
        text = remove_member(&text, &outline, second);
    }
}

/// `text` with the first entry named `name` taken out, `None` when there is
/// none.
fn remove_first(text: &str, name: &str) -> Result<Option<String>, JsonEntryError> {
    let outline = outline(text)?;
    Ok(outline
        .members
        .iter()
        .position(|member| member.key == name)
        .map(|index| remove_member(text, &outline, index)))
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

fn scan_object(source: &str) -> Option<Outline> {
    let bytes = source.as_bytes();
    let open = skip_whitespace(bytes, 0);
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
