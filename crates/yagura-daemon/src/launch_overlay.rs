//! The launch overlay: a settings layer a window's CLI is handed at launch,
//! which keeps the window's credentials (and the overlay itself) away from
//! the tools the CLI runs, while every setting the user wrote stays theirs.
//!
//! What the overlay is built from is profile data (`[launch_overlay]`): the
//! variables the CLI reads an overlay from, the settings layers below the
//! overlay, and the list of variable names its tools are kept from. One pure
//! function ([`build_overlay`]) builds it from the facts of the environment
//! the CLI runs in, whether the daemon read them itself (a CLI it runs
//! directly) or the window's overlay door sent them (a CLI in another
//! environment).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};
use thiserror::Error;
use yagura_protocol::{STATUS_TOKEN_ENV, WINDOW_ID_ENV};

/// The most the overlay door reads from its stdin.
///
/// Its request carries the input as JSON strings, where one byte takes at
/// most six (`\u00XX`), so eight times this stays below an IPC frame
/// ([`yagura_protocol::MAX_FRAME_BODY_LEN`]) with room for the rest of the
/// request.
pub const MAX_DOOR_INPUT_BYTES: usize = yagura_protocol::MAX_FRAME_BODY_LEN / 8;

/// A profile's validated `[launch_overlay]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchOverlaySpec {
    /// The variable the CLI reads an inline overlay from, and the one a
    /// window's CLI is handed its overlay in.
    pub env: String,
    /// The variable naming a file the CLI reads an overlay from when no
    /// inline one is usable: JSON when its name ends in `.json`, TOML
    /// otherwise.
    pub file_env: String,
    /// The variable naming the directory the settings layers are in.
    pub dir_env: Option<String>,
    /// That directory relative to the home directory, when `dir_env` names
    /// none.
    pub dir: String,
    /// The settings layers below the overlay, each a file under the
    /// directory, or an absolute path starting with `/`.
    pub layers: Vec<String>,
    /// The JSON Pointer of the list of variable names the CLI keeps from its
    /// tools, in a layer and in the overlay alike.
    pub list: String,
}

/// Why a `[launch_overlay]` table was rejected.
#[derive(Debug, Clone, Error)]
pub enum LaunchOverlayError {
    /// The table does not decode: a key it does not take, or a value of the
    /// wrong type.
    #[error("the launch overlay table does not decode")]
    Shape,
    /// A variable name is empty, holds `=` or NUL, is the daemon's own
    /// (`YAGURA_`) or names the same variable as another.
    #[error("a launch overlay variable name is not one the table may name")]
    Variable,
    /// `dir` or a layer is not a plain path: empty, `..`, a backslash, a
    /// drive, or an absolute layer not starting with `/`.
    #[error("a launch overlay path is not a plain path")]
    Path,
    /// `list` is not a JSON Pointer naming a key.
    #[error("the launch overlay list is not a JSON Pointer")]
    List,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSpec {
    env: String,
    file_env: String,
    #[serde(default)]
    dir_env: Option<String>,
    dir: String,
    layers: Vec<String>,
    list: String,
}

/// Validates a `[launch_overlay]` table.
///
/// # Errors
///
/// Returns [`LaunchOverlayError`] for the rule the table breaks.
pub fn parse_spec(raw: toml::Value) -> Result<LaunchOverlaySpec, LaunchOverlayError> {
    let raw: RawSpec = raw.try_into().map_err(|_| LaunchOverlayError::Shape)?;
    let mut names = vec![&raw.env, &raw.file_env];
    names.extend(raw.dir_env.as_ref());
    let distinct = names.iter().enumerate().all(|(at, name)| {
        !names[..at]
            .iter()
            .any(|other| other.eq_ignore_ascii_case(name))
    });
    if !distinct || !names.iter().all(|name| is_variable_name(name)) {
        return Err(LaunchOverlayError::Variable);
    }
    if !is_plain_relative(&raw.dir)
        || raw.layers.is_empty()
        || !raw.layers.iter().all(|layer| {
            layer
                .strip_prefix('/')
                .map_or_else(|| is_plain_relative(layer), is_plain_relative)
        })
        // A layer's text and the overlay file's are sent under these names.
        || raw.layers.contains(&raw.file_env)
    {
        return Err(LaunchOverlayError::Path);
    }
    if !raw.list.starts_with('/')
        || raw.list.len() < 2
        || !crate::profile::is_json_pointer(&raw.list)
    {
        return Err(LaunchOverlayError::List);
    }
    Ok(LaunchOverlaySpec {
        env: raw.env,
        file_env: raw.file_env,
        dir_env: raw.dir_env,
        dir: raw.dir,
        layers: raw.layers,
        list: raw.list,
    })
}

fn is_variable_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['=', '\0'])
        && !name.to_ascii_uppercase().starts_with("YAGURA_")
}

/// A path of plain names joined by `/`: nothing that climbs out or names a
/// drive.
fn is_plain_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// What an overlay is built from: the values of the variables the profile
/// names, and the text of each layer and overlay file, by its name in the
/// profile (a layer as the profile writes it, the overlay file as the
/// variable naming it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlayFacts {
    /// The variables, by name.
    pub env: BTreeMap<String, String>,
    /// The files, by name.
    pub files: BTreeMap<String, String>,
}

/// The facts of the environment the daemon itself runs in, which is the one
/// a CLI it runs directly sees: `env` reads a variable, and the files are read
/// from `home`. Absolute layers name Unix paths, read only where
/// `reads_absolute` (a Windows host has no file of the CLI's there).
#[must_use]
pub fn host_facts(
    spec: &LaunchOverlaySpec,
    home: Option<&Path>,
    reads_absolute: bool,
    env: &dyn Fn(&str) -> Option<String>,
) -> OverlayFacts {
    let mut facts = OverlayFacts::default();
    for name in [&spec.env, &spec.file_env] {
        if let Some(value) = env(name) {
            facts.env.insert(name.clone(), value);
        }
    }
    let dir: Option<PathBuf> = spec
        .dir_env
        .as_deref()
        .and_then(env)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|home| home.join(&spec.dir)));
    for layer in &spec.layers {
        let path = if layer.starts_with('/') {
            reads_absolute.then(|| PathBuf::from(layer))
        } else {
            dir.as_ref().map(|dir| dir.join(layer))
        };
        if let Some(text) = path.as_deref().and_then(read_text) {
            facts.files.insert(layer.clone(), text);
        }
    }
    if let Some(path) = facts
        .env
        .get(&spec.file_env)
        .filter(|path| !path.is_empty())
        && let Some(text) = read_text(Path::new(path))
    {
        facts.files.insert(spec.file_env.clone(), text);
    }
    facts
}

/// The text of `path`; `None` when it has none to read, which a missing file
/// is not worth a word about.
fn read_text(path: &Path) -> Option<String> {
    match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => Some(text),
            Err(_) => {
                tracing::warn!("a settings file of a launch overlay is not UTF-8; it is left out");
                None
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            tracing::warn!(%error, "a settings file of a launch overlay could not be read; it is left out");
            None
        }
    }
}

/// An overlay, and what was left out of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltOverlay {
    /// The overlay as a JSON object's text.
    pub text: String,
    /// Each thing the user wrote that could not be read, so the overlay keeps
    /// nothing of it; fixed sentences that hold no value.
    pub left_out: Vec<&'static str>,
}

/// Builds the overlay `spec` describes from `facts`.
///
/// The overlay is the user's own overlay as the CLI would take it (the inline
/// one when it reads as a JSON object, else the overlay file), with its list
/// replaced by every name the layers below it, the user's overlay, and the
/// window keep from the tools: the window's credentials and the overlay's
/// own variables, which may hold what was only in the user's files. Whatever
/// cannot be read is left out, never the window's names.
#[must_use]
pub fn build_overlay(spec: &LaunchOverlaySpec, facts: &OverlayFacts) -> BuiltOverlay {
    let mut left_out = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for layer in &spec.layers {
        if let Some(text) = facts.files.get(layer) {
            match parse_document(layer, text) {
                Some(document) => names.extend(listed(&document, &spec.list)),
                None => left_out.push("a settings layer below the overlay does not read"),
            }
        }
    }
    let inline = facts
        .env
        .get(&spec.env)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let document = serde_json::from_str::<Value>(value)
                .ok()
                .filter(Value::is_object);
            if document.is_none() {
                left_out.push("the inline overlay is not a JSON object");
            }
            document
        });
    let user = match inline.flatten() {
        Some(document) => Some(document),
        None => facts
            .env
            .get(&spec.file_env)
            .filter(|path| !path.is_empty())
            .and_then(|path| {
                let document = facts
                    .files
                    .get(&spec.file_env)
                    .and_then(|text| parse_document(path, text))
                    .filter(Value::is_object);
                if document.is_none() {
                    left_out.push("the overlay file does not read as an object");
                }
                document
            }),
    };
    let mut overlay = user.unwrap_or_else(|| Value::Object(Map::new()));
    names.extend(listed(&overlay, &spec.list));
    names.extend(
        [WINDOW_ID_ENV, STATUS_TOKEN_ENV, &spec.env, &spec.file_env]
            .into_iter()
            .map(str::to_string),
    );
    let mut kept: Vec<Value> = Vec::new();
    for name in names {
        let name = Value::String(name);
        if !kept.contains(&name) {
            kept.push(name);
        }
    }
    set_at(&mut overlay, &spec.list, Value::Array(kept));
    BuiltOverlay {
        // A JSON value always serializes.
        text: serde_json::to_string(&overlay).unwrap_or_default(),
        left_out,
    }
}

/// `text` read as JSON when `name` ends in `.json`, as TOML otherwise.
fn parse_document(name: &str, text: &str) -> Option<Value> {
    if Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
    {
        serde_json::from_str(text).ok()
    } else {
        text.parse::<toml::Table>()
            .ok()
            .map(|table| toml_to_json(toml::Value::Table(table)))
    }
}

fn toml_to_json(value: toml::Value) -> Value {
    match value {
        toml::Value::String(text) => Value::String(text),
        toml::Value::Integer(number) => Value::from(number),
        toml::Value::Float(number) => {
            serde_json::Number::from_f64(number).map_or(Value::Null, Value::Number)
        }
        toml::Value::Boolean(flag) => Value::Bool(flag),
        toml::Value::Datetime(datetime) => Value::String(datetime.to_string()),
        toml::Value::Array(items) => Value::Array(items.into_iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => Value::Object(
            table
                .into_iter()
                .map(|(key, value)| (key, toml_to_json(value)))
                .collect(),
        ),
    }
}

/// The strings of the list at `pointer` in `document`.
fn listed(document: &Value, pointer: &str) -> Vec<String> {
    document
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Puts `value` at `pointer` in `document`, turning whatever stands on the
/// way into an object.
fn set_at(document: &mut Value, pointer: &str, value: Value) {
    let tokens: Vec<String> = pointer
        .split('/')
        .skip(1)
        .map(|token| token.replace("~1", "/").replace("~0", "~"))
        .collect();
    let Some((last, path)) = tokens.split_last() else {
        return;
    };
    let mut at = document;
    for token in path {
        if !at.is_object() {
            *at = Value::Object(Map::new());
        }
        let Value::Object(map) = at else { return };
        at = map
            .entry(token.clone())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    if !at.is_object() {
        *at = Value::Object(Map::new());
    }
    if let Value::Object(map) = at {
        map.insert(last.clone(), value);
    }
}

/// Why what the overlay door was given on its stdin was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DoorInputError {
    /// More than [`MAX_DOOR_INPUT_BYTES`].
    #[error("more input than the overlay door reads")]
    TooLarge,
    /// Not records of the door's form.
    #[error("the input is not in the overlay door's form")]
    Form,
}

/// Reads what a launch statement hands the overlay door on its stdin: records
/// of three NUL-terminated fields, the kind (`env` or `file`), the name the
/// profile gives it, and its value, which a shell writes with `printf` and
/// `cat` alone. A name given twice, or text that is not UTF-8, is refused.
///
/// # Errors
///
/// Returns [`DoorInputError`] for input over the limit or out of form.
pub fn read_door_input(input: &[u8]) -> Result<OverlayFacts, DoorInputError> {
    if input.len() > MAX_DOOR_INPUT_BYTES {
        return Err(DoorInputError::TooLarge);
    }
    let mut facts = OverlayFacts::default();
    if input.is_empty() {
        return Ok(facts);
    }
    let fields: Vec<&[u8]> = input
        .strip_suffix(b"\0")
        .ok_or(DoorInputError::Form)?
        .split(|byte| *byte == 0)
        .collect();
    if !fields.len().is_multiple_of(3) {
        return Err(DoorInputError::Form);
    }
    for record in fields.chunks(3) {
        let text =
            |bytes: &[u8]| String::from_utf8(bytes.to_vec()).map_err(|_| DoorInputError::Form);
        let (name, value) = (text(record[1])?, text(record[2])?);
        let into = match record[0] {
            b"env" => &mut facts.env,
            b"file" => &mut facts.files,
            _ => return Err(DoorInputError::Form),
        };
        if name.is_empty() || into.insert(name, value).is_some() {
            return Err(DoorInputError::Form);
        }
    }
    Ok(facts)
}
