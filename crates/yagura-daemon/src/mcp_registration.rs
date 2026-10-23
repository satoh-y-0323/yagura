//! The `[mcp_registration]` launch profile table: how the daemon registers
//! its own MCP bridge with a CLI outside any window, and how it reads back
//! whether that registration is still in place.
//!
//! Everything here is I/O-free: parsing validates a table read from a
//! profile document, the `render_*` methods turn it into argv the caller
//! spawns, and [`derive_status`] turns a captured command's output (or a
//! read file's content) into one of five states. No vendor name appears
//! anywhere in this module; the difference between CLIs lives only in the
//! table's own values.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value as JsonValue;
use thiserror::Error;
use yagura_os::program::{self, HostOs, Platform};

use crate::host_paths::{FileTools, HostPaths};
use crate::model_choices::{COMMAND_NOT_FOUND_STATUS, output_lines, parse_json_document};
use crate::profile;
use crate::toml_set::TableEdit;

/// The most bytes [`sanitize_output`] keeps of one operation step's output.
pub const STEP_OUTPUT_MAX_BYTES: usize = 16 * 1024;

/// Why a `[mcp_registration]` table was rejected. Carried only as the
/// `source()` of [`crate::profile::ProfileParseError::InvalidMcpRegistration`],
/// so it reaches the daemon's log without appearing in that variant's fixed,
/// document-independent `Display`.
#[derive(Debug, Clone, Error)]
pub enum McpRegistrationError {
    /// The table failed to decode: an unknown key, a missing required key,
    /// or a value of the wrong shape.
    #[error("could not be parsed")]
    Decode(#[from] toml::de::Error),
    /// The table decoded, but breaks a rule this module enforces itself (an
    /// unrecognized placeholder, an invalid JSON Pointer, a settings-file
    /// path leaving the home directory, an invalid `group` heading, or a
    /// `status` naming neither or both of `args` / `file`).
    #[error("the table breaks its rule")]
    InvalidShape,
}

/// Why [`McpRegistrationSpec::render_add`] could not render argv.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RenderError {
    /// The target names an instance (its environment is non-empty), but the
    /// table has no `env_arg` to carry it -- registering would silently
    /// reach the default instance's daemon instead.
    #[error("this profile's registration table cannot carry the instance name")]
    InstanceNotCarried,
}

/// The name the default instance registers under, and the prefix of every
/// named instance's.
const DEFAULT_SERVER: &str = "yagura";

/// The registration name and environment one instance registers under.
#[derive(Debug, Clone)]
pub struct RegistrationTarget {
    server: String,
    env: Vec<(String, String)>,
}

impl RegistrationTarget {
    /// The target for `instance_id`: `None` (the default instance) registers
    /// as `yagura` with no environment; `Some(id)` registers as
    /// `yagura-<id>` and carries `YAGURA_INSTANCE=<id>` -- so a development
    /// instance never overwrites, or is overwritten by, the default
    /// instance's own registration.
    #[must_use]
    pub fn for_instance(instance_id: Option<&str>) -> Self {
        match instance_id {
            None => Self {
                server: DEFAULT_SERVER.to_string(),
                env: Vec::new(),
            },
            Some(id) => Self {
                server: format!("{DEFAULT_SERVER}-{id}"),
                env: vec![("YAGURA_INSTANCE".to_string(), id.to_string())],
            },
        }
    }

    /// Whether `name` is one [`RegistrationTarget::for_instance`] can produce:
    /// `yagura`, or `yagura-` followed by a valid instance identifier.
    #[must_use]
    pub fn is_bridge_server(name: &str) -> bool {
        match name.strip_prefix(DEFAULT_SERVER) {
            Some("") => true,
            Some(rest) => rest.strip_prefix('-').is_some_and(|id| {
                yagura_os::validate_instance_id(std::ffi::OsStr::new(id)).is_ok()
            }),
            None => false,
        }
    }

    /// The registration name.
    #[must_use]
    pub fn server(&self) -> &str {
        &self.server
    }

    /// The environment variables this instance's bridge needs, in the order
    /// `render_add` carries them.
    #[must_use]
    pub fn env(&self) -> &[(String, String)] {
        &self.env
    }
}

/// Where a `status` reads its answer from.
#[derive(Debug, Clone)]
enum StatusSource {
    /// Run `args` (with `{server}` substituted) and read its output.
    Command(Vec<String>),
    /// Read the content of a settings file (`{file}`-templated tools read
    /// and replace it; a plain home-relative path is read directly).
    File(String),
}

/// How a `status`'s output is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusFormat {
    Json,
    Lines,
}

/// A validated `[mcp_registration.status]` table.
#[derive(Debug, Clone)]
struct StatusSpec {
    source: StatusSource,
    format: StatusFormat,
    /// `json`: a JSON Pointer (may contain `{server}`) to the registered
    /// executable. `lines`: the line prefix preceding it.
    command: String,
    /// `json` only: a JSON Pointer to the registered arguments, checked for
    /// being empty.
    args_pointer: Option<String>,
    /// `lines` only: line prefix -> the word its value must start with.
    require: BTreeMap<String, String>,
    /// `args`-sourced status only: a substring whose presence in the output
    /// means "not registered".
    absent: Option<String>,
}

/// A validated `env_forward` table.
#[derive(Debug, Clone)]
struct EnvForward {
    name: String,
    separator: String,
}

/// A validated `[mcp_registration.toml_set]` table: an in-place edit made to
/// an existing table of a settings file once registration succeeds, plus the
/// tables of that file the registration owns outright.
#[derive(Debug, Clone)]
pub struct TomlSetSpec {
    file: String,
    table: Vec<String>,
    values: toml::Table,
    confirm: BTreeMap<String, String>,
    entries: Option<EntriesSpec>,
}

/// A validated `toml_set.entries` table: tables registration creates under
/// `under` (one per key, holding that key's values) and removal deletes,
/// read back from the file itself. The set differs by the operating system
/// the CLI runs on, chosen the way `[command]`'s argv is.
#[derive(Debug, Clone)]
struct EntriesSpec {
    under: Vec<String>,
    default: Option<toml::Table>,
    windows: Option<toml::Table>,
    macos: Option<toml::Table>,
    linux: Option<toml::Table>,
}

impl TomlSetSpec {
    /// The settings file's path, home-relative.
    #[must_use]
    pub fn file(&self) -> &str {
        &self.file
    }

    /// The table path to edit, with `{server}` substituted.
    #[must_use]
    pub fn table(&self, target: &RegistrationTarget) -> Vec<String> {
        substitute_all(&self.table, "{server}", target.server())
    }

    /// The keys and values to set, exactly as the profile declared them.
    #[must_use]
    pub fn values(&self) -> &toml::Table {
        &self.values
    }

    /// Key -> JSON Pointer (may contain `{server}`) checked, once the edit
    /// is made, against the corresponding key in [`TomlSetSpec::values`].
    pub(crate) fn confirm(&self) -> &BTreeMap<String, String> {
        &self.confirm
    }

    /// Whether this edit owns tables of its own (`toml_set.entries`).
    #[must_use]
    pub fn has_entries(&self) -> bool {
        self.entries.is_some()
    }

    /// The name [`DerivedStatus::differs_in`] gives the owned tables: the
    /// dotted path they live under.
    #[must_use]
    pub fn entries_label(&self) -> Option<String> {
        self.entries.as_ref().map(|entries| entries.under.join("."))
    }

    /// Every edit registration makes for `target` on `os`: the existing
    /// table's values first, then each owned table (created when missing).
    #[must_use]
    pub fn edits(&self, target: &RegistrationTarget, os: HostOs) -> Vec<TableEdit<'_>> {
        let mut edits = vec![TableEdit {
            table: self.table(target),
            values: &self.values,
            create: false,
        }];
        edits.extend(self.entry_edits(os));
        edits
    }

    /// The owned tables on `os`, as edits that create them. Empty when this
    /// edit owns none, or none for `os`.
    #[must_use]
    pub fn entry_edits(&self, os: HostOs) -> Vec<TableEdit<'_>> {
        let Some(entries) = &self.entries else {
            return Vec::new();
        };
        let Some(selected) = program::select_for_os(
            os,
            entries.windows.as_ref(),
            entries.macos.as_ref(),
            entries.linux.as_ref(),
            entries.default.as_ref(),
        ) else {
            return Vec::new();
        };
        selected
            .iter()
            .filter_map(|(key, value)| {
                let mut table = entries.under.clone();
                table.push(key.clone());
                Some(TableEdit {
                    table,
                    values: value.as_table()?,
                    create: true,
                })
            })
            .collect()
    }
}

const SERVER_PLACEHOLDER: &str = "{server}";

/// The placeholder that stands for a whole object value: the variables a
/// window passes to the bridge, each named for the CLI to fill in.
const ENV_REFS_PLACEHOLDER: &str = "{env_refs}";

/// The one entry of a `json_entry` that a registration's CLI reads the
/// bridge from.
#[derive(Debug, Clone)]
struct BridgeEntry {
    /// JSON Pointer to the object holding every instance's bridge entry; empty
    /// when they are top-level entries of the document.
    items: String,
    /// JSON Pointer, inside one such entry, to the string holding `{bridge}`.
    command: String,
    /// Whether the entry carries the instance's variables (`{env_refs}`).
    carries_env: bool,
}

/// The entry a registration is refused over when it already holds another
/// value, and whose own value marks the shared entries as this
/// registration's.
#[derive(Debug, Clone)]
pub struct Claim {
    file: String,
    pointer: String,
    value: JsonValue,
}

impl Claim {
    /// The home-relative path of the file holding the claimed entry.
    #[must_use]
    pub fn file(&self) -> &str {
        &self.file
    }

    /// The claimed entry, as the profile spells it.
    #[must_use]
    pub fn pointer(&self) -> &str {
        &self.pointer
    }

    /// Whether `document` holds a value at the claimed entry other than the
    /// one this registration writes.
    #[must_use]
    pub fn is_held_by_another(&self, document: &JsonValue) -> bool {
        document
            .pointer(&as_pointer(&self.pointer))
            .is_some_and(|held| *held != self.value)
    }

    /// Whether `document` holds exactly the value this registration writes.
    #[must_use]
    pub fn is_held_by_us(&self, document: &JsonValue) -> bool {
        document.pointer(&as_pointer(&self.pointer)) == Some(&self.value)
    }
}

/// A path a `json_entry` value names that the caller has none for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingPath {
    /// `{daemon}`.
    Daemon,
    /// `{bridge}`.
    Bridge,
}

/// What a `json_entry` value is rendered with.
#[derive(Debug, Clone, Copy)]
pub struct EntryVars<'a> {
    /// The instance the entries are written for (`{server}`, `{env_refs}`).
    pub target: &'a RegistrationTarget,
    /// The operating system the CLI runs on (the `by_os` set to use).
    pub os: HostOs,
    /// `{daemon}`: the path a hook command runs the daemon by.
    pub daemon: Option<&'a str>,
    /// `{bridge}`: the path to the bridge.
    pub bridge: Option<&'a str>,
}

/// Entries per operating system, chosen the way `[command]`'s argv is.
#[derive(Debug, Clone, Default)]
struct EntriesByOs {
    default: Option<Vec<(String, JsonValue)>>,
    windows: Option<Vec<(String, JsonValue)>>,
    macos: Option<Vec<(String, JsonValue)>>,
    linux: Option<Vec<(String, JsonValue)>>,
}

impl EntriesByOs {
    fn selected(&self, os: HostOs) -> &[(String, JsonValue)] {
        program::select_for_os(
            os,
            self.windows.as_deref(),
            self.macos.as_deref(),
            self.linux.as_deref(),
            self.default.as_deref(),
        )
        .unwrap_or_default()
    }

    fn all(&self) -> impl Iterator<Item = &(String, JsonValue)> {
        [&self.default, &self.windows, &self.macos, &self.linux]
            .into_iter()
            .flatten()
            .flatten()
    }
}

/// One settings file a `json_entry` writes: the entries it owns there, and
/// the entry (if any) it claims.
#[derive(Debug, Clone)]
pub struct JsonFileSpec {
    file: String,
    entries: Vec<(String, JsonValue)>,
    by_os: EntriesByOs,
    claim: Option<Claim>,
}

impl JsonFileSpec {
    /// The settings file's path, home-relative.
    #[must_use]
    pub fn file(&self) -> &str {
        &self.file
    }

    /// The entry this file claims, when it does.
    #[must_use]
    pub fn claim(&self) -> Option<&Claim> {
        self.claim.as_ref()
    }

    /// Every entry written on `os`: the `entries` first, then the chosen
    /// `by_os` set.
    fn selected(&self, os: HostOs) -> impl Iterator<Item = &(String, JsonValue)> {
        self.entries.iter().chain(self.by_os.selected(os))
    }

    /// Whether the file has entries to write on `os`.
    #[must_use]
    pub fn owns_entries_on(&self, os: HostOs) -> bool {
        self.selected(os).next().is_some()
    }

    /// Whether an entry written on `os` names the daemon.
    #[must_use]
    pub fn names_daemon(&self, os: HostOs) -> bool {
        self.selected(os)
            .any(|(_, value)| json_contains(value, "{daemon}"))
    }

    /// Whether an entry written on `os` names the bridge.
    #[must_use]
    pub fn names_bridge(&self, os: HostOs) -> bool {
        self.selected(os)
            .any(|(_, value)| json_contains(value, "{bridge}"))
    }

    /// The entries written on `vars.os`, each key and value rendered.
    ///
    /// # Errors
    ///
    /// Returns the [`MissingPath`] of the first path a value names that
    /// `vars` has none for.
    pub fn entries(&self, vars: &EntryVars<'_>) -> Result<Vec<(String, JsonValue)>, MissingPath> {
        self.selected(vars.os)
            .map(|(key, value)| Ok((fill(key, vars, false)?, render_json(value, vars, false)?)))
            .collect()
    }

    /// [`JsonFileSpec::entries`] for display: a path `vars` has none for stays
    /// as its placeholder.
    #[must_use]
    pub fn preview(&self, vars: &EntryVars<'_>) -> Vec<(String, JsonValue)> {
        self.selected(vars.os)
            .filter_map(|(key, value)| {
                Some((
                    fill(key, vars, true).ok()?,
                    render_json(value, vars, true).ok()?,
                ))
            })
            .collect()
    }

    /// The keys a removal takes out on `os`: the entries of this instance
    /// alone, plus the shared ones when `shared` says they go too.
    #[must_use]
    pub fn removal_keys(
        &self,
        target: &RegistrationTarget,
        os: HostOs,
        shared: bool,
    ) -> Vec<String> {
        self.selected(os)
            .filter(|(key, _)| shared || key.contains(SERVER_PLACEHOLDER))
            .map(|(key, _)| key.replace(SERVER_PLACEHOLDER, target.server()))
            .collect()
    }

    /// The name [`DerivedStatus::differs_in`] gives this file's entries.
    #[must_use]
    pub fn label(&self, target: &RegistrationTarget, os: HostOs) -> String {
        self.removal_keys(target, os, true).join(", ")
    }
}

/// A validated `[[mcp_registration.json_entry]]` array: entries of JSON
/// settings files the registration owns outright -- added by registration,
/// taken out by removal, read back from the files themselves. A key starting
/// with `/` is a JSON Pointer, any other a top-level name; both may carry
/// `{server}`. String values may carry `{daemon}` (the path a hook command
/// runs the daemon by), `{bridge}` and `{server}`; a value that is exactly
/// `{env_refs}` stands for the variables a window passes to the bridge.
///
/// Entries without `{server}` are shared by every instance. One of them is
/// the `claim`: while the file holds another value there, nothing is written
/// or taken out of the shared entries.
#[derive(Debug, Clone)]
pub struct JsonEntrySpec {
    files: Vec<JsonFileSpec>,
    bridge_entry: Option<BridgeEntry>,
}

impl JsonEntrySpec {
    /// The files, in the order the profile declares them.
    #[must_use]
    pub fn files(&self) -> &[JsonFileSpec] {
        &self.files
    }

    /// The claim, when the registration has one.
    #[must_use]
    pub fn claim(&self) -> Option<&Claim> {
        self.files.iter().find_map(JsonFileSpec::claim)
    }

    /// Whether a window of a named instance can reach its own bridge through
    /// the entry the registration writes for it.
    #[must_use]
    pub fn carries_instance(&self) -> bool {
        self.bridge_entry
            .as_ref()
            .is_some_and(|entry| entry.carries_env)
    }

    /// The file holding the bridge entries of every instance.
    #[must_use]
    pub fn bridge_file(&self) -> Option<&str> {
        self.files
            .iter()
            .find(|file| {
                file.entries
                    .iter()
                    .any(|(key, value)| is_bridge_entry(key, value))
            })
            .map(JsonFileSpec::file)
    }

    /// The names, in the order `document` holds them, of the registered
    /// servers that are yagura's bridge for some instance other than the one
    /// named `keep`. Decided the way [`ListSpec::other_bridges`] decides it.
    ///
    /// `None` when the registration has no bridge entry, or `document` holds
    /// something other than an object where the bridge entries belong.
    #[must_use]
    pub fn other_bridges(
        &self,
        document: &JsonValue,
        bridge_file_name: &str,
        keep: &str,
    ) -> Option<Vec<String>> {
        let entry = self.bridge_entry.as_ref()?;
        let Some(items) = document.pointer(&entry.items) else {
            return Some(Vec::new());
        };
        let named = items
            .as_object()?
            .iter()
            .map(|(name, item)| (name.as_str(), item));
        Some(other_bridge_names(
            named,
            &entry.command,
            bridge_file_name,
            keep,
        ))
    }
}

/// The key every instance's bridge entry has: the instance's name as the last
/// token of the pointer.
fn is_bridge_key(key: &str) -> bool {
    key == SERVER_PLACEHOLDER || key.ends_with(&format!("/{SERVER_PLACEHOLDER}"))
}

/// Whether `key`'s entry is the one that registers the bridge: it is named
/// for the instance and holds `{bridge}` as a string of its own.
fn is_bridge_entry(key: &str, value: &JsonValue) -> bool {
    is_bridge_key(key) && bridge_pointer(value).is_some()
}

/// The JSON Pointer to the string that is exactly `{bridge}` inside `value`.
fn bridge_pointer(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(text) => (text == "{bridge}").then(String::new),
        JsonValue::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(index, item)| Some(format!("/{index}{}", bridge_pointer(item)?))),
        JsonValue::Object(map) => map.iter().find_map(|(key, item)| {
            let token = key.replace('~', "~0").replace('/', "~1");
            Some(format!("/{token}{}", bridge_pointer(item)?))
        }),
        _ => None,
    }
}

/// `key` as a JSON Pointer: a key starting with `/` already is one, any other
/// names a top-level entry.
fn as_pointer(key: &str) -> String {
    if key.starts_with('/') {
        key.to_string()
    } else {
        format!("/{}", key.replace('~', "~0").replace('/', "~1"))
    }
}

/// Whether a string `value` holds contains `needle`.
fn json_contains(value: &JsonValue, needle: &str) -> bool {
    match value {
        JsonValue::String(text) => text.contains(needle),
        JsonValue::Array(items) => items.iter().any(|item| json_contains(item, needle)),
        JsonValue::Object(map) => map.values().any(|item| json_contains(item, needle)),
        _ => false,
    }
}

/// The bridge's environment in a registered entry, which every session of the
/// CLI reads, inside a window or not. A window's identity is a reference that
/// falls back to empty, so a session outside a window still starts its MCP
/// servers (an unset plain reference disables them all); the instance is the
/// value itself, as the command rows carry it.
fn env_refs(target: &RegistrationTarget) -> JsonValue {
    let identity = [
        yagura_protocol::WINDOW_ID_ENV,
        yagura_protocol::STATUS_TOKEN_ENV,
    ]
    .map(|name| {
        (
            name.to_string(),
            JsonValue::String(format!("${{{name}:-}}")),
        )
    });
    let instance = target
        .env()
        .iter()
        .map(|(name, value)| (name.clone(), JsonValue::String(value.clone())));
    JsonValue::Object(identity.into_iter().chain(instance).collect())
}

/// `text` with `{server}`, `{daemon}` and `{bridge}` replaced in one pass, so
/// a substituted path that spells a placeholder is never rewritten. A path
/// `vars` has none for is an error, or stays as its placeholder when `lossy`.
fn fill(text: &str, vars: &EntryVars<'_>, lossy: bool) -> Result<String, MissingPath> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let (before, after) = rest.split_at(open);
        out.push_str(before);
        let hit = [
            (SERVER_PLACEHOLDER, Some(vars.target.server()), None),
            ("{daemon}", vars.daemon, Some(MissingPath::Daemon)),
            ("{bridge}", vars.bridge, Some(MissingPath::Bridge)),
        ]
        .into_iter()
        .find(|(placeholder, _, _)| after.starts_with(placeholder));
        let Some((placeholder, value, missing)) = hit else {
            out.push('{');
            rest = after.strip_prefix('{').unwrap_or_default();
            continue;
        };
        match (value, missing) {
            (Some(value), _) => out.push_str(value),
            (None, Some(missing)) if !lossy => return Err(missing),
            (None, _) => out.push_str(placeholder),
        }
        rest = after.strip_prefix(placeholder).unwrap_or_default();
    }
    out.push_str(rest);
    Ok(out)
}

/// `value` with every placeholder in its strings replaced ([`fill`]), and a
/// string that is exactly `{env_refs}` replaced by the object it stands for.
fn render_json(
    value: &JsonValue,
    vars: &EntryVars<'_>,
    lossy: bool,
) -> Result<JsonValue, MissingPath> {
    Ok(match value {
        JsonValue::String(text) if text == ENV_REFS_PLACEHOLDER => env_refs(vars.target),
        JsonValue::String(text) => JsonValue::String(fill(text, vars, lossy)?),
        JsonValue::Array(items) => JsonValue::Array(
            items
                .iter()
                .map(|item| render_json(item, vars, lossy))
                .collect::<Result<_, _>>()?,
        ),
        JsonValue::Object(map) => JsonValue::Object(
            map.iter()
                .map(|(key, item)| Ok((key.clone(), render_json(item, vars, lossy)?)))
                .collect::<Result<_, MissingPath>>()?,
        ),
        other => other.clone(),
    })
}

/// A validated `[mcp_registration.list]` table: how to read every server the
/// CLI has registered, and how to turn one of them off for a single launch.
#[derive(Debug, Clone)]
pub struct ListSpec {
    args: Vec<String>,
    items: String,
    name: ItemName,
    command: String,
    disable: Vec<String>,
}

/// Where a listed server's name is read from.
#[derive(Debug, Clone)]
enum ItemName {
    /// `items` is an array; the name is at this JSON Pointer inside each
    /// element.
    Pointer(String),
    /// `items` is a table; each member is one server, named by its key.
    Key,
}

impl ListSpec {
    /// The arguments (after the CLI's own launch argv) that list the
    /// registrations.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// The names, in the order the listing holds them, of the registered
    /// servers that are yagura's bridge for some instance other than the one
    /// named `keep`: the name is one [`RegistrationTarget::is_bridge_server`]
    /// accepts and the registered command's file name is `bridge_file_name`.
    /// A server of the user's own, or a bridge-named one that runs something
    /// else, is never in the result.
    ///
    /// `None` when `stdout` is not the declared shape: not UTF-8, not JSON,
    /// or `items` does not point at an array (at a table, when the names are
    /// its keys).
    #[must_use]
    pub fn other_bridges(
        &self,
        stdout: &[u8],
        bridge_file_name: &str,
        keep: &str,
    ) -> Option<Vec<String>> {
        let document = parse_json_document(std::str::from_utf8(stdout).ok()?)?;
        let items = document.pointer(&self.items)?;
        let named: Vec<(&str, &JsonValue)> = match &self.name {
            ItemName::Pointer(pointer) => items
                .as_array()?
                .iter()
                .filter_map(|item| Some((item.pointer(pointer)?.as_str()?, item)))
                .collect(),
            ItemName::Key => items
                .as_object()?
                .iter()
                .map(|(name, item)| (name.as_str(), item))
                .collect(),
        };
        Some(other_bridge_names(
            named.into_iter(),
            &self.command,
            bridge_file_name,
            keep,
        ))
    }

    /// The launch arguments that turn `servers` off, in order.
    #[must_use]
    pub fn render_disable(&self, servers: &[String]) -> Vec<String> {
        servers
            .iter()
            .flat_map(|server| substitute_all(&self.disable, "{server}", server))
            .collect()
    }
}

/// The names, in order, of the servers in `named` that are yagura's bridge
/// for some instance other than `keep`: the name is one
/// [`RegistrationTarget::is_bridge_server`] accepts and the command found at
/// the JSON Pointer `command` inside the server ends in `bridge_file_name`. A
/// server of the user's own, or a bridge-named one that runs something else,
/// is never in the result.
fn other_bridge_names<'a>(
    named: impl Iterator<Item = (&'a str, &'a JsonValue)>,
    command: &str,
    bridge_file_name: &str,
    keep: &str,
) -> Vec<String> {
    named
        .filter_map(|(name, item)| {
            let registered = item.pointer(command)?.as_str()?;
            let file_name = registered.rsplit(['/', '\\']).next().unwrap_or(registered);
            (name != keep
                && RegistrationTarget::is_bridge_server(name)
                && file_name.eq_ignore_ascii_case(bridge_file_name))
            .then(|| name.to_string())
        })
        .collect()
}

/// Whether every string `value` holds is free of placeholders outside
/// `allowed`.
fn json_placeholders_allowed(value: &JsonValue, allowed: &[&str]) -> bool {
    match value {
        JsonValue::String(text) => !profile::contains_placeholder_outside(text, allowed),
        JsonValue::Array(items) => items
            .iter()
            .all(|item| json_placeholders_allowed(item, allowed)),
        JsonValue::Object(map) => map.iter().all(|(key, item)| {
            !profile::contains_placeholder_outside(key, &[])
                && json_placeholders_allowed(item, allowed)
        }),
        _ => true,
    }
}

/// A validated `[mcp_registration]` table: how to register, unregister and
/// read back the daemon's own MCP bridge with one CLI.
#[derive(Debug, Clone)]
pub struct McpRegistrationSpec {
    group: Option<String>,
    commands: Option<RegistrationCommands>,
    env_arg: Option<Vec<String>>,
    env_forward: Option<EnvForward>,
    host_paths: Option<HostPaths>,
    /// The row's own `files` table, or else its profile's `[host_paths]`
    /// file tools: the one way this row reads and writes the CLI's files.
    files: Option<FileTools>,
    status: StatusSpec,
    toml_set: Option<TomlSetSpec>,
    json_entry: Option<JsonEntrySpec>,
    list: Option<ListSpec>,
}

/// The CLI's own commands for registering and unregistering a server. A
/// registration without them is made entirely by editing settings files.
#[derive(Debug, Clone)]
struct RegistrationCommands {
    add: Vec<String>,
    remove: Vec<String>,
}

impl McpRegistrationSpec {
    /// The heading this row is shown under in the GUI (`None` for the host
    /// environment's own group).
    #[must_use]
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// The `toml_set` table, when this row edits a settings file after
    /// registering.
    #[must_use]
    pub fn toml_set(&self) -> Option<&TomlSetSpec> {
        self.toml_set.as_ref()
    }

    /// The `json_entry` table, when this row owns entries of a JSON settings
    /// file.
    #[must_use]
    pub fn json_entry(&self) -> Option<&JsonEntrySpec> {
        self.json_entry.as_ref()
    }

    /// The `list` table, when this row can name the CLI's registered servers
    /// and turn one off for a launch.
    #[must_use]
    pub fn list(&self) -> Option<&ListSpec> {
        self.list.as_ref()
    }

    /// Whether this row owns settings-file entries of its own (a `toml_set`
    /// with `entries`, or a `json_entry`): what a launch of the profile
    /// depends on for its CLI to run yagura's hooks.
    #[must_use]
    pub fn owns_entries(&self) -> bool {
        self.toml_set.as_ref().is_some_and(TomlSetSpec::has_entries) || self.json_entry.is_some()
    }

    /// Whether this row's profile converts daemon-side paths through its
    /// `[host_paths]` tool ([`McpRegistrationSpec::render_probe`]) rather
    /// than using them directly.
    #[must_use]
    pub fn converts_paths(&self) -> bool {
        self.host_paths.is_some()
    }

    /// Whether the CLI has commands that register and unregister a server;
    /// without them a registration only edits settings files.
    #[must_use]
    pub fn has_commands(&self) -> bool {
        self.commands.is_some()
    }

    /// Checks that `target`'s registration can carry the instance it names:
    /// by `env_arg` when the CLI registers through a command, by the
    /// `{env_refs}` of the bridge entry when it is written to a settings file.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::InstanceNotCarried`] when it cannot.
    pub fn check_target(&self, target: &RegistrationTarget) -> Result<(), RenderError> {
        if self.commands.is_some() {
            return self.render_env_args(target).map(drop);
        }
        let carried = self
            .json_entry
            .as_ref()
            .is_some_and(JsonEntrySpec::carries_instance);
        if target.env().is_empty() || carried {
            Ok(())
        } else {
            Err(RenderError::InstanceNotCarried)
        }
    }

    /// Renders the command that registers `target` with `bridge`; empty when
    /// the table has no commands.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::InstanceNotCarried`] when `target` names an
    /// instance but the table has no `env_arg` to carry its environment.
    pub fn render_add(
        &self,
        target: &RegistrationTarget,
        bridge: &str,
    ) -> Result<Vec<String>, RenderError> {
        let Some(commands) = &self.commands else {
            return Ok(Vec::new());
        };
        let env_args = self.render_env_args(target)?;
        let mut argv = Vec::with_capacity(commands.add.len());
        for element in &commands.add {
            if element == "{env_args}" {
                argv.extend(env_args.iter().cloned());
            } else {
                argv.push(
                    element
                        .replace("{server}", target.server())
                        .replace("{bridge}", bridge),
                );
            }
        }
        Ok(argv)
    }

    /// Renders the command that unregisters `target`; empty when the table has
    /// no commands.
    #[must_use]
    pub fn render_remove(&self, target: &RegistrationTarget) -> Vec<String> {
        self.commands.as_ref().map_or_else(Vec::new, |commands| {
            substitute_all(&commands.remove, "{server}", target.server())
        })
    }

    /// Renders how `target`'s status should be read: either a command to
    /// run or a settings file to read.
    #[must_use]
    pub fn render_status(&self, target: &RegistrationTarget) -> StatusPlan {
        match &self.status.source {
            StatusSource::Command(args) => {
                StatusPlan::Run(substitute_all(args, "{server}", target.server()))
            }
            StatusSource::File(file) => StatusPlan::ReadFile { file: file.clone() },
        }
    }

    /// Renders the command that turns the daemon-side `path` into this
    /// environment's own path to it (the profile's `[host_paths]`), `None`
    /// when the profile has no such tool (it uses `path` directly).
    #[must_use]
    pub fn render_probe(&self, path: &str) -> Option<Vec<String>> {
        self.host_paths
            .as_ref()
            .map(|host_paths| host_paths.render(path))
    }

    /// The tools this row reads and replaces settings files with, `None`
    /// when it reads them directly on the host.
    #[must_use]
    pub fn file_tools(&self) -> Option<&FileTools> {
        self.files.as_ref()
    }

    /// Renders the command that reads `file`'s content, `None` when the row
    /// reads settings files directly rather than through a tool.
    #[must_use]
    pub fn render_file_read(&self, file: &str) -> Option<Vec<String>> {
        self.files.as_ref().map(|files| files.render_read(file))
    }

    /// Renders the command that replaces `file`'s content (the new content
    /// is passed on its standard input), `None` when the row has no such
    /// tool.
    #[must_use]
    pub fn render_file_replace(&self, file: &str) -> Option<Vec<String>> {
        self.files
            .as_ref()
            .and_then(|files| files.render_replace(file))
    }

    /// The exit code the row's read tool uses for "the file is not there",
    /// when it declares one.
    #[must_use]
    pub fn files_absent_exit(&self) -> Option<i32> {
        self.files.as_ref().and_then(FileTools::absent_exit)
    }

    /// Like [`McpRegistrationSpec::render_add`], but never fails: a table
    /// that cannot carry `target`'s environment is rendered as if `target`
    /// carried none, for previewing a row a caller has already decided not
    /// to run. Running the row still goes through `render_add` and is
    /// refused there.
    #[must_use]
    pub fn render_add_lossy(&self, target: &RegistrationTarget, bridge: &str) -> Vec<String> {
        match self.render_add(target, bridge) {
            Ok(argv) => argv,
            Err(RenderError::InstanceNotCarried) => {
                let bare = RegistrationTarget {
                    server: target.server.clone(),
                    env: Vec::new(),
                };
                self.render_add(&bare, bridge).unwrap_or_default()
            }
        }
    }

    /// The environment pairs `{env_args}` expands to: `target`'s own pairs,
    /// then -- once `target` names an instance and the table has
    /// `env_forward` -- one more pair carrying the forwarded variable names,
    /// each rendered through `env_arg`.
    fn render_env_args(&self, target: &RegistrationTarget) -> Result<Vec<String>, RenderError> {
        let mut pairs: Vec<(&str, String)> = target
            .env()
            .iter()
            .map(|(name, value)| (name.as_str(), value.clone()))
            .collect();
        if !target.env().is_empty()
            && let Some(forward) = &self.env_forward
        {
            let names: Vec<&str> = target.env().iter().map(|(name, _)| name.as_str()).collect();
            pairs.push((forward.name.as_str(), names.join(&forward.separator)));
        }
        if pairs.is_empty() {
            return Ok(Vec::new());
        }
        let Some(env_arg) = &self.env_arg else {
            return Err(RenderError::InstanceNotCarried);
        };
        let mut rendered = Vec::with_capacity(env_arg.len() * pairs.len());
        for (name, value) in &pairs {
            let pair = format!("{name}={value}");
            rendered.extend(substitute_all(env_arg, "{env}", &pair));
        }
        Ok(rendered)
    }
}

/// Replaces every occurrence of `placeholder` in each of `args` with
/// `value`, leaving every other character untouched.
fn substitute_all(args: &[String], placeholder: &str, value: &str) -> Vec<String> {
    args.iter()
        .map(|arg| arg.replace(placeholder, value))
        .collect()
}

/// How [`McpRegistrationSpec::render_status`] says a status should be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusPlan {
    /// Run this command and read its output.
    Run(Vec<String>),
    /// Read this settings file's content (home-relative path).
    ReadFile {
        /// The file's path, home-relative.
        file: String,
    },
}

/// What reading a status came back with, independent of how it was read (a
/// command's exit, or a settings file's content).
#[derive(Debug, Clone)]
pub enum StatusObservation {
    /// The CLI itself could not be found, or the row's probe tool failed.
    NotInstalled,
    /// The settings file this row reads directly does not exist.
    FileMissing,
    /// A command ran to completion (or was killed after producing this much
    /// output).
    Exited {
        /// The exit code, `None` when the process ended without reporting
        /// one.
        code: Option<i32>,
        /// Standard output, or a settings file's content read through a
        /// tool.
        stdout: Vec<u8>,
        /// Standard error.
        stderr: Vec<u8>,
    },
    /// The command ran past its time limit and was stopped.
    TimedOut,
    /// The command wrote more output than accepted.
    OutputTooLarge,
}

/// The five states a registration row can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationState {
    /// The CLI's own record names exactly this bridge, with every required
    /// value in place.
    Registered,
    /// The CLI's own record exists, but names a different bridge or has a
    /// required value this daemon did not set.
    Differs,
    /// The CLI's own record has no such registration.
    NotRegistered,
    /// The CLI itself is not present.
    NotInstalled,
    /// The status could not be read (an unreadable format, a timeout, an
    /// output over the limit, or an unrecognized failure).
    Unknown,
}

/// What [`derive_status`] found: the state, which parts (if any) differ from
/// what this daemon would register, and why an [`RegistrationState::Unknown`]
/// state could not be read further -- a fixed phrase naming no path and
/// quoting no output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedStatus {
    /// The state.
    pub state: RegistrationState,
    /// The names of the parts that differ (`"command"`, a `require` key with
    /// its trailing `:` stripped, or a `toml_set.confirm` key), empty unless
    /// `state` is [`RegistrationState::Differs`].
    pub differs_in: Vec<String>,
    /// Why the state is [`RegistrationState::Unknown`], `None` otherwise.
    pub reason: Option<String>,
}

const REASON_UNREADABLE_OUTPUT: &str = "the output could not be read as the profile's declared \
                                         format";
const REASON_COMMAND_UNREADABLE: &str =
    "the registered executable could not be read from the output";

fn not_installed() -> DerivedStatus {
    DerivedStatus {
        state: RegistrationState::NotInstalled,
        differs_in: Vec::new(),
        reason: Some("the executable was not found".to_string()),
    }
}

fn not_registered() -> DerivedStatus {
    DerivedStatus {
        state: RegistrationState::NotRegistered,
        differs_in: Vec::new(),
        reason: None,
    }
}

fn unknown(reason: &str) -> DerivedStatus {
    DerivedStatus {
        state: RegistrationState::Unknown,
        differs_in: Vec::new(),
        reason: Some(reason.to_string()),
    }
}

/// The state a row settles into when its [`McpRegistrationSpec::render_probe`]
/// command succeeded but its output failed [`validate_probe_output`]:
/// readable, but not a path this daemon can compare a registration against.
#[must_use]
pub fn probe_output_unusable() -> DerivedStatus {
    unknown("the environment probe returned an unusable path")
}

/// Derives a row's [`RegistrationState`] from `observed`, comparing what the
/// CLI itself reports against `expected_bridge` (the bridge this daemon
/// would register) for `target`.
///
/// `platform` decides how the registered command path is compared to
/// `expected_bridge` when the row's profile has no `[host_paths]` (Windows
/// compares case-insensitively and normalizes `/` to `\`; every other host
/// compares exactly). A row whose profile converts paths always compares
/// exactly, on every host: the conversion already rendered the path the way
/// that environment itself sees it.
#[must_use]
pub fn derive_status(
    spec: &McpRegistrationSpec,
    observed: &StatusObservation,
    target: &RegistrationTarget,
    expected_bridge: &str,
    platform: Platform,
) -> DerivedStatus {
    match observed {
        StatusObservation::NotInstalled => not_installed(),
        StatusObservation::FileMissing => not_registered(),
        StatusObservation::TimedOut => unknown("the command did not finish within its time limit"),
        StatusObservation::OutputTooLarge => {
            unknown("the command's output was larger than accepted")
        }
        StatusObservation::Exited {
            code,
            stdout,
            stderr,
        } => derive_from_exit(
            spec,
            *code,
            stdout,
            stderr,
            target,
            expected_bridge,
            platform,
        ),
    }
}

fn derive_from_exit(
    spec: &McpRegistrationSpec,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
    target: &RegistrationTarget,
    expected_bridge: &str,
    platform: Platform,
) -> DerivedStatus {
    if code == Some(COMMAND_NOT_FOUND_STATUS) {
        return not_installed();
    }
    match &spec.status.source {
        StatusSource::File(_) => {
            derive_file_status(spec, code, stdout, target, expected_bridge, platform)
        }
        StatusSource::Command(_) => derive_command_status(
            spec,
            code,
            stdout,
            stderr,
            target,
            expected_bridge,
            platform,
        ),
    }
}

fn derive_command_status(
    spec: &McpRegistrationSpec,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
    target: &RegistrationTarget,
    expected_bridge: &str,
    platform: Platform,
) -> DerivedStatus {
    if let Some(absent) = &spec.status.absent {
        let stdout_text = String::from_utf8_lossy(stdout);
        let stderr_text = String::from_utf8_lossy(stderr);
        if stdout_text.contains(absent.as_str()) || stderr_text.contains(absent.as_str()) {
            return not_registered();
        }
    }
    if code != Some(0) {
        return unknown(
            "the command exited in a way this profile does not recognize as registered or not",
        );
    }
    derive_from_output(spec, stdout, target, expected_bridge, platform, false)
}

fn derive_file_status(
    spec: &McpRegistrationSpec,
    code: Option<i32>,
    stdout: &[u8],
    target: &RegistrationTarget,
    expected_bridge: &str,
    platform: Platform,
) -> DerivedStatus {
    let absent_exit = spec.files_absent_exit();
    if absent_exit.is_some() && code == absent_exit {
        return not_registered();
    }
    if code != Some(0) {
        return unknown("the settings file could not be read");
    }
    derive_from_output(spec, stdout, target, expected_bridge, platform, true)
}

/// Reads the registered executable (and, for `json`, `toml_set.confirm`)
/// from `stdout`, common to a command's standard output and a settings
/// file's content. `is_file_source` decides whether "the value is simply not
/// there" means [`RegistrationState::NotRegistered`] (a settings file) or [`RegistrationState::Unknown`] (a command,
/// whose own `absent` phrase is how "not registered" is normally spelled).
fn derive_from_output(
    spec: &McpRegistrationSpec,
    stdout: &[u8],
    target: &RegistrationTarget,
    expected_bridge: &str,
    platform: Platform,
    is_file_source: bool,
) -> DerivedStatus {
    let status = &spec.status;
    let has_probe = spec.converts_paths();
    let Ok(text) = std::str::from_utf8(stdout) else {
        return unknown(REASON_UNREADABLE_OUTPUT);
    };

    // A settings file a CLI leaves empty (or whitespace-only) is how that CLI
    // itself spells "no registration", not an unreadable format. A command's
    // own output being blank carries no such meaning, so this applies only to
    // file sources.
    if is_file_source && text.trim().is_empty() {
        return not_registered();
    }

    let mut differs_in = Vec::new();
    let document = match status.format {
        StatusFormat::Json => {
            let Some(document) = parse_json_document(text) else {
                return unknown(REASON_UNREADABLE_OUTPUT);
            };
            let pointer = status.command.replace("{server}", target.server());
            let actual = match document.pointer(&pointer) {
                None if is_file_source => return not_registered(),
                None => return unknown(REASON_COMMAND_UNREADABLE),
                Some(value) => match value.as_str() {
                    Some(actual) => actual,
                    None => return unknown(REASON_COMMAND_UNREADABLE),
                },
            };
            if command_matches(expected_bridge, actual, has_probe, platform) {
                if let Some(args_pointer) = &status.args_pointer {
                    let leftover = document
                        .pointer(args_pointer)
                        .and_then(JsonValue::as_array)
                        .is_some_and(|args| !args.is_empty());
                    if leftover {
                        differs_in.push("command".to_string());
                    }
                }
            } else {
                differs_in.push("command".to_string());
            }
            Some(document)
        }
        StatusFormat::Lines => {
            let lines: Vec<&str> = output_lines(text).collect();
            let actual = match find_labeled_value(&lines, &status.command) {
                Some(actual) => actual,
                None if is_file_source => return not_registered(),
                None => return unknown(REASON_COMMAND_UNREADABLE),
            };
            if !command_matches(expected_bridge, actual, has_probe, platform) {
                differs_in.push("command".to_string());
            }
            for (label, expected) in &status.require {
                let satisfied = find_labeled_value(&lines, label)
                    .is_some_and(|value| value.starts_with(expected.as_str()));
                if !satisfied {
                    differs_in.push(strip_label(label));
                }
            }
            None
        }
    };

    if status.format == StatusFormat::Json
        && let Some(document) = &document
        && let Some(toml_set) = spec.toml_set()
    {
        for (key, pointer) in toml_set.confirm() {
            let pointer = pointer.replace("{server}", target.server());
            let matches = match (toml_set.values().get(key), document.pointer(&pointer)) {
                (Some(expected), Some(actual)) => json_value_matches(expected, actual),
                _ => false,
            };
            if !matches {
                differs_in.push(key.clone());
            }
        }
    }

    let state = if differs_in.is_empty() {
        RegistrationState::Registered
    } else {
        RegistrationState::Differs
    };
    DerivedStatus {
        state,
        differs_in,
        reason: None,
    }
}

/// Finds the first line (trimmed) starting with `label`, returning the rest
/// of it, trimmed. Every line is trimmed before comparison, so indentation
/// never keeps a label from matching.
fn find_labeled_value<'a>(lines: &[&'a str], label: &str) -> Option<&'a str> {
    lines
        .iter()
        .find_map(|line| line.trim().strip_prefix(label).map(str::trim))
}

/// A `require` key (for example `"Scope:"`) as it appears in
/// [`DerivedStatus::differs_in`]: its trailing `:` and surrounding
/// whitespace dropped.
fn strip_label(label: &str) -> String {
    label.trim_end_matches(':').trim().to_string()
}

/// Whether `actual` (read from the CLI's own status output) names the same
/// executable as `expected_bridge` (what this daemon would register).
fn command_matches(expected: &str, actual: &str, has_probe: bool, platform: Platform) -> bool {
    if has_probe {
        // A probe already rendered `expected_bridge` the way this
        // environment itself sees it, so no further normalization applies,
        // regardless of the host operating system.
        return expected == actual;
    }
    match platform {
        Platform::Windows => normalize_windows_path(expected) == normalize_windows_path(actual),
        Platform::Unix => expected == actual,
    }
}

/// Normalizes a Windows path for comparison: `/` folded to `\`, lower-cased.
fn normalize_windows_path(path: &str) -> String {
    path.replace('/', "\\").to_ascii_lowercase()
}

/// Compares a `toml_set.values` entry against the JSON value a status read
/// back, treating any TOML integer and float as equal to the JSON number
/// with the same numeric value (a CLI may round-trip an integer setting as
/// a float). Strings and booleans compare only within their own JSON type.
/// A TOML array, table or datetime never matches (`toml_set` values are
/// scalars in every profile this module knows).
#[must_use]
pub fn json_value_matches(expected: &toml::Value, actual: &JsonValue) -> bool {
    match expected {
        toml::Value::Integer(n) => actual.as_f64().is_some_and(|actual| actual == *n as f64),
        toml::Value::Float(n) => actual.as_f64().is_some_and(|actual| actual == *n),
        toml::Value::String(s) => actual.as_str().is_some_and(|actual| actual == s),
        toml::Value::Boolean(b) => actual.as_bool().is_some_and(|actual| actual == *b),
        toml::Value::Datetime(_) | toml::Value::Array(_) | toml::Value::Table(_) => false,
    }
}

/// The most bytes [`validate_probe_output`] accepts.
const PROBE_OUTPUT_MAX_BYTES: usize = 4 * 1024;

/// Validates a `[host_paths]` conversion's standard output as a usable path:
/// exactly one line (its trailing newline, and a preceding `\r`, are
/// dropped), starting with `/`, holding no control character and no more
/// than [`PROBE_OUTPUT_MAX_BYTES`]. `None` when any of that does not hold,
/// or the output is not UTF-8.
#[must_use]
pub fn validate_probe_output(stdout: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(stdout).ok()?;
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
    if trimmed.is_empty() || !trimmed.starts_with('/') {
        return None;
    }
    if trimmed.len() > PROBE_OUTPUT_MAX_BYTES {
        return None;
    }
    if trimmed.chars().any(|ch| ch.is_control()) {
        return None;
    }
    Some(trimmed.to_string())
}

/// Whether `ch` is dropped by [`sanitize_output`]: every C0 control
/// character except `\n`/`\t`, the C1 range, and the Unicode bidi format
/// characters that can be used to disguise the direction text reads in.
fn is_dropped_control(ch: char) -> bool {
    match ch {
        '\n' | '\t' => false,
        '\u{0000}'..='\u{001f}' | '\u{007f}'..='\u{009f}' => true,
        '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => true,
        _ => false,
    }
}

/// Cleans a captured operation step's output for display: ANSI escape
/// sequences (`ESC [ ... final-byte`) and [`is_dropped_control`]'s
/// characters are dropped, invalid UTF-8 is replaced (`U+FFFD`), and the
/// result is cut at `max_bytes` without splitting a multi-byte character.
#[must_use]
pub fn sanitize_output(bytes: &[u8], max_bytes: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }
        if is_dropped_control(ch) {
            continue;
        }
        out.push(ch);
    }
    truncate_at_char_boundary(&out, max_bytes)
}

/// `text` cut to at most `max_bytes` bytes, rounded down to the nearest
/// character boundary.
fn truncate_at_char_boundary(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

// ---------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------

/// A `[mcp_registration]` table before validation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMcpRegistrationTable {
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    add: Option<Vec<String>>,
    #[serde(default)]
    remove: Option<Vec<String>>,
    #[serde(default)]
    env_arg: Option<Vec<String>>,
    #[serde(default)]
    env_forward: Option<RawEnvForward>,
    #[serde(default)]
    files: Option<RawFiles>,
    status: RawStatusTable,
    #[serde(default)]
    toml_set: Option<RawTomlSet>,
    #[serde(default)]
    json_entry: Option<RawJsonEntries>,
    #[serde(default)]
    list: Option<RawList>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawList {
    args: Vec<String>,
    items: String,
    #[serde(default)]
    name: Option<String>,
    command: String,
    disable: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEnvForward {
    name: String,
    separator: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFiles {
    read: Vec<String>,
    #[serde(default)]
    replace: Option<Vec<String>>,
    #[serde(default)]
    absent_exit: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStatusTable {
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    file: Option<String>,
    format: String,
    command: String,
    #[serde(default)]
    args_pointer: Option<String>,
    #[serde(default)]
    require: Option<BTreeMap<String, String>>,
    #[serde(default)]
    absent: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTomlSet {
    file: String,
    table: Vec<String>,
    values: toml::Table,
    confirm: BTreeMap<String, String>,
    #[serde(default)]
    entries: Option<RawEntries>,
}

/// `json_entry` as a single table or an array of tables.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawJsonEntries {
    One(RawJsonEntry),
    Many(Vec<RawJsonEntry>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJsonEntry {
    file: String,
    #[serde(default)]
    entries: toml::Table,
    #[serde(default)]
    by_os: Option<RawByOs>,
    #[serde(default)]
    claim: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawByOs {
    #[serde(default)]
    default: Option<toml::Table>,
    #[serde(default)]
    windows: Option<toml::Table>,
    #[serde(default)]
    macos: Option<toml::Table>,
    #[serde(default)]
    linux: Option<toml::Table>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntries {
    under: Vec<String>,
    #[serde(default)]
    default: Option<toml::Table>,
    #[serde(default)]
    windows: Option<toml::Table>,
    #[serde(default)]
    macos: Option<toml::Table>,
    #[serde(default)]
    linux: Option<toml::Table>,
}

/// Parses `raw` (the profile's own `[mcp_registration]` table, still
/// untyped) into a [`McpRegistrationSpec`] that converts daemon-side paths
/// through `host_paths` (the profile's own `[host_paths]`). `None` in, `None`
/// out.
///
/// # Errors
///
/// Returns [`McpRegistrationError`] for a table that fails to decode (an
/// unknown or missing key, a value of the wrong shape) or breaks a rule this
/// module enforces itself (see [`McpRegistrationError::InvalidShape`]).
pub(crate) fn parse_mcp_registration(
    raw: Option<toml::Value>,
    host_paths: Option<&HostPaths>,
) -> Result<Option<McpRegistrationSpec>, McpRegistrationError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let table: RawMcpRegistrationTable = raw.try_into()?;
    build_spec(table, host_paths.cloned()).map(Some)
}

fn build_spec(
    raw: RawMcpRegistrationTable,
    host_paths: Option<HostPaths>,
) -> Result<McpRegistrationSpec, McpRegistrationError> {
    let invalid = || McpRegistrationError::InvalidShape;

    if let Some(group) = &raw.group {
        yagura_protocol::validate_display_label(group).map_err(|_| invalid())?;
    }

    let commands = match (raw.add, raw.remove) {
        (Some(add), Some(remove)) => {
            validate_add_placeholders(&add)?;
            validate_args_placeholders(&remove, &["{server}"])?;
            Some(RegistrationCommands { add, remove })
        }
        (None, None) => None,
        _ => return Err(invalid()),
    };
    // Without commands there is no argv for the environment to ride on.
    if commands.is_none() && (raw.env_arg.is_some() || raw.env_forward.is_some()) {
        return Err(invalid());
    }
    if let Some(env_arg) = &raw.env_arg {
        validate_args_placeholders(env_arg, &["{env}"])?;
    }

    let host_tools = host_paths.as_ref().and_then(HostPaths::file_tools);
    let files = match raw.files {
        // One way to read the CLI's files, never two that could disagree.
        Some(_) if host_tools.is_some() => return Err(invalid()),
        Some(raw_files) => Some(
            FileTools::new(raw_files.read, raw_files.replace, raw_files.absent_exit)
                .map_err(|_| invalid())?,
        ),
        None => host_tools.cloned(),
    };

    let status = build_status(raw.status)?;

    let toml_set = match raw.toml_set {
        Some(raw_toml_set) => {
            if !is_safe_relative_home_path(&raw_toml_set.file) {
                return Err(invalid());
            }
            validate_args_placeholders(&raw_toml_set.table, &["{server}"])?;
            if raw_toml_set
                .confirm
                .values()
                .any(|pointer| !profile::is_json_pointer(pointer))
            {
                return Err(invalid());
            }
            let entries = raw_toml_set.entries.map(build_entries).transpose()?;
            Some(TomlSetSpec {
                file: raw_toml_set.file,
                table: raw_toml_set.table,
                values: raw_toml_set.values,
                confirm: raw_toml_set.confirm,
                entries,
            })
        }
        None => None,
    };

    let json_entry = raw.json_entry.map(build_json_entry).transpose()?;
    // Registered by editing files alone, the bridge entry is the registration.
    if commands.is_none()
        && json_entry
            .as_ref()
            .is_none_or(|entry| entry.bridge_entry.is_none())
    {
        return Err(invalid());
    }
    let list = raw.list.map(build_list).transpose()?;

    Ok(McpRegistrationSpec {
        group: raw.group,
        commands,
        env_arg: raw.env_arg,
        env_forward: raw.env_forward.map(|forward| EnvForward {
            name: forward.name,
            separator: forward.separator,
        }),
        host_paths,
        files,
        status,
        toml_set,
        json_entry,
        list,
    })
}

/// Validates `list`: arguments without placeholders, JSON Pointers for the
/// items and the fields read from each of them (no `name` when the items are
/// a table keyed by name), and a `disable` that names its server by
/// `{server}` alone.
fn build_list(raw: RawList) -> Result<ListSpec, McpRegistrationError> {
    let invalid = || McpRegistrationError::InvalidShape;
    validate_args_placeholders(&raw.args, &[])?;
    validate_args_placeholders(&raw.disable, &["{server}"])?;
    let pointers = [Some(&raw.items), raw.name.as_ref(), Some(&raw.command)];
    if raw.args.is_empty()
        || !raw.disable.iter().any(|arg| arg.contains("{server}"))
        || !pointers
            .iter()
            .flatten()
            .all(|pointer| profile::is_json_pointer(pointer))
    {
        return Err(invalid());
    }
    Ok(ListSpec {
        args: raw.args,
        items: raw.items,
        name: raw.name.map_or(ItemName::Key, ItemName::Pointer),
        command: raw.command,
        disable: raw.disable,
    })
}

/// Validates `json_entry`: every file safe and home-relative with at least one
/// entry, keys carrying no placeholder but `{server}`, values carrying none
/// but `{daemon}`, `{bridge}`, `{server}` and a whole-value `{env_refs}`.
///
/// Entries without `{server}` are shared by every instance, so they need a
/// claim (at most one, over a shared entry holding a plain value) and a
/// bridge entry to tell whether another instance is still registered.
fn build_json_entry(raw: RawJsonEntries) -> Result<JsonEntrySpec, McpRegistrationError> {
    let invalid = || McpRegistrationError::InvalidShape;
    let elements = match raw {
        RawJsonEntries::One(element) => vec![element],
        RawJsonEntries::Many(elements) => elements,
    };
    if elements.is_empty() {
        return Err(invalid());
    }
    let files = elements
        .into_iter()
        .map(build_json_file)
        .collect::<Result<Vec<_>, _>>()?;
    if files.iter().filter(|file| file.claim.is_some()).count() > 1 {
        return Err(invalid());
    }
    let bridge_entry = files
        .iter()
        .flat_map(|file| &file.entries)
        .find(|(key, value)| is_bridge_entry(key, value))
        .and_then(|(key, value)| {
            let command = bridge_pointer(value)?;
            let items = key.strip_suffix(SERVER_PLACEHOLDER)?;
            Some(BridgeEntry {
                items: items.strip_suffix('/').unwrap_or(items).to_string(),
                command,
                carries_env: json_contains(value, ENV_REFS_PLACEHOLDER),
            })
        });
    let shared = files
        .iter()
        .flat_map(|file| file.entries.iter().chain(file.by_os.all()))
        .any(|(key, _)| !key.contains(SERVER_PLACEHOLDER));
    if shared && (bridge_entry.is_none() || files.iter().all(|file| file.claim.is_none())) {
        return Err(invalid());
    }
    Ok(JsonEntrySpec {
        files,
        bridge_entry,
    })
}

/// Validates one `json_entry` element (see [`build_json_entry`]).
fn build_json_file(raw: RawJsonEntry) -> Result<JsonFileSpec, McpRegistrationError> {
    let invalid = || McpRegistrationError::InvalidShape;
    if !is_safe_relative_home_path(&raw.file) {
        return Err(invalid());
    }
    let entries = build_json_entries(raw.entries)?;
    let by_os = match raw.by_os {
        Some(by_os) => EntriesByOs {
            default: by_os.default.map(build_json_entries).transpose()?,
            windows: by_os.windows.map(build_json_entries).transpose()?,
            macos: by_os.macos.map(build_json_entries).transpose()?,
            linux: by_os.linux.map(build_json_entries).transpose()?,
        },
        None => EntriesByOs::default(),
    };
    if entries.is_empty() && by_os.all().next().is_none() {
        return Err(invalid());
    }
    // A set chosen for some operating system must not rewrite an entry the
    // file always writes.
    let overlaps = [HostOs::Windows, HostOs::MacOs, HostOs::Linux]
        .into_iter()
        .any(|os| {
            by_os
                .selected(os)
                .iter()
                .any(|(key, _)| entries.iter().any(|(own, _)| own == key))
        });
    if overlaps {
        return Err(invalid());
    }
    let claim = raw
        .claim
        .map(|pointer| {
            let (key, value) = entries
                .iter()
                .find(|(key, _)| *key == pointer)
                .ok_or_else(invalid)?;
            // A shared entry holding a plain value: both sides must be able
            // to compare it without knowing an instance or a path.
            if key.contains(SERVER_PLACEHOLDER) || !json_placeholders_allowed(value, &[]) {
                return Err(invalid());
            }
            Ok(Claim {
                file: raw.file.clone(),
                pointer,
                value: value.clone(),
            })
        })
        .transpose()?;
    Ok(JsonFileSpec {
        file: raw.file,
        entries,
        by_os,
        claim,
    })
}

/// Validates the entries of one table of a `json_entry` element.
fn build_json_entries(raw: toml::Table) -> Result<Vec<(String, JsonValue)>, McpRegistrationError> {
    let invalid = || McpRegistrationError::InvalidShape;
    let mut entries = Vec::with_capacity(raw.len());
    for (key, value) in raw {
        let value = serde_json::to_value(value).map_err(|_| invalid())?;
        if profile::contains_placeholder_outside(&key, &[SERVER_PLACEHOLDER])
            || !json_placeholders_allowed(
                &value,
                &[
                    SERVER_PLACEHOLDER,
                    "{daemon}",
                    "{bridge}",
                    ENV_REFS_PLACEHOLDER,
                ],
            )
            || !env_refs_stand_alone(&value)
        {
            return Err(invalid());
        }
        entries.push((key, value));
    }
    Ok(entries)
}

/// Whether every string of `value` that mentions `{env_refs}` is exactly it:
/// it stands for an object, which cannot be spliced into a larger string.
fn env_refs_stand_alone(value: &JsonValue) -> bool {
    match value {
        JsonValue::String(text) => {
            !text.contains(ENV_REFS_PLACEHOLDER) || text == ENV_REFS_PLACEHOLDER
        }
        JsonValue::Array(items) => items.iter().all(env_refs_stand_alone),
        JsonValue::Object(map) => map.values().all(env_refs_stand_alone),
        _ => true,
    }
}

/// Validates `toml_set.entries`: a non-empty `under` without placeholders,
/// at least one operating-system set, and every entry a non-empty table.
fn build_entries(raw: RawEntries) -> Result<EntriesSpec, McpRegistrationError> {
    let invalid = || McpRegistrationError::InvalidShape;
    if raw.under.is_empty() {
        return Err(invalid());
    }
    validate_args_placeholders(&raw.under, &[])?;
    let sets = [&raw.default, &raw.windows, &raw.macos, &raw.linux];
    if sets.iter().all(|set| set.is_none()) {
        return Err(invalid());
    }
    let well_formed = sets.iter().filter_map(|set| set.as_ref()).all(|set| {
        set.values()
            .all(|entry| entry.as_table().is_some_and(|table| !table.is_empty()))
    });
    if !well_formed {
        return Err(invalid());
    }
    Ok(EntriesSpec {
        under: raw.under,
        default: raw.default,
        windows: raw.windows,
        macos: raw.macos,
        linux: raw.linux,
    })
}

fn build_status(raw: RawStatusTable) -> Result<StatusSpec, McpRegistrationError> {
    let invalid = || McpRegistrationError::InvalidShape;
    let format = match raw.format.as_str() {
        "json" => StatusFormat::Json,
        "lines" => StatusFormat::Lines,
        _ => return Err(invalid()),
    };
    let source = match (raw.args, raw.file) {
        (Some(args), None) => {
            validate_args_placeholders(&args, &["{server}"])?;
            StatusSource::Command(args)
        }
        (None, Some(file)) => {
            if !is_safe_relative_home_path(&file) {
                return Err(invalid());
            }
            StatusSource::File(file)
        }
        _ => return Err(invalid()),
    };
    if format == StatusFormat::Json {
        if !profile::is_json_pointer(&raw.command) {
            return Err(invalid());
        }
        if let Some(pointer) = &raw.args_pointer
            && !profile::is_json_pointer(pointer)
        {
            return Err(invalid());
        }
    }
    Ok(StatusSpec {
        source,
        format,
        command: raw.command,
        args_pointer: raw.args_pointer,
        require: raw.require.unwrap_or_default(),
        absent: raw.absent,
    })
}

/// Validates `add`'s elements: `{server}` and `{bridge}` anywhere, and
/// `{env_args}` only as a whole element (spliced into a larger element it
/// could not be rendered as argv).
fn validate_add_placeholders(add: &[String]) -> Result<(), McpRegistrationError> {
    for element in add {
        if element.contains("{env_args}") {
            if element != "{env_args}" {
                return Err(McpRegistrationError::InvalidShape);
            }
            continue;
        }
        if profile::contains_placeholder_outside(element, &["{server}", "{bridge}"]) {
            return Err(McpRegistrationError::InvalidShape);
        }
    }
    Ok(())
}

/// Validates that every element of `args` carries no placeholder outside
/// `allowed`.
fn validate_args_placeholders(
    args: &[String],
    allowed: &[&str],
) -> Result<(), McpRegistrationError> {
    for element in args {
        if profile::contains_placeholder_outside(element, allowed) {
            return Err(McpRegistrationError::InvalidShape);
        }
    }
    Ok(())
}

/// Whether `path` is safe to join under the home directory: relative, and
/// never climbing above it via a `..` segment. Both `/` and `\` are
/// accepted as separators, and a Windows drive letter is rejected, so the
/// check does not depend on the host operating system parsing the profile
/// (a `C:\...` path must be rejected even when the daemon runs on Linux).
pub(crate) fn is_safe_relative_home_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.starts_with('\\') || path.contains(':') {
        return false;
    }
    path.split(['/', '\\']).all(|segment| segment != "..")
}
