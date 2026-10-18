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
                server: "yagura".to_string(),
                env: Vec::new(),
            },
            Some(id) => Self {
                server: format!("yagura-{id}"),
                env: vec![("YAGURA_INSTANCE".to_string(), id.to_string())],
            },
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

/// A validated `files` table.
#[derive(Debug, Clone)]
struct FilesSpec {
    read: Vec<String>,
    replace: Option<Vec<String>>,
    absent_exit: Option<i32>,
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

/// A validated `[mcp_registration.json_entry]` table: top-level entries of a
/// JSON settings file the registration owns outright -- added by
/// registration, taken out by removal, read back from the file itself.
/// Entry names may carry `{server}`; string values may carry `{daemon}`,
/// the path a hook command runs the daemon by.
#[derive(Debug, Clone)]
pub struct JsonEntrySpec {
    file: String,
    entries: Vec<(String, JsonValue)>,
}

impl JsonEntrySpec {
    /// The settings file's path, home-relative.
    #[must_use]
    pub fn file(&self) -> &str {
        &self.file
    }

    /// The entry names for `target`.
    #[must_use]
    pub fn names(&self, target: &RegistrationTarget) -> Vec<String> {
        self.entries
            .iter()
            .map(|(name, _)| name.replace("{server}", target.server()))
            .collect()
    }

    /// The entries for `target`, with `daemon` in place of `{daemon}`.
    #[must_use]
    pub fn entries(&self, target: &RegistrationTarget, daemon: &str) -> Vec<(String, JsonValue)> {
        self.names(target)
            .into_iter()
            .zip(&self.entries)
            .map(|(name, (_, value))| (name, substitute_json(value, "{daemon}", daemon)))
            .collect()
    }
}

/// `value` with `placeholder` replaced by `with` in every string it holds.
fn substitute_json(value: &JsonValue, placeholder: &str, with: &str) -> JsonValue {
    match value {
        JsonValue::String(text) => JsonValue::String(text.replace(placeholder, with)),
        JsonValue::Array(items) => JsonValue::Array(
            items
                .iter()
                .map(|item| substitute_json(item, placeholder, with))
                .collect(),
        ),
        JsonValue::Object(map) => JsonValue::Object(
            map.iter()
                .map(|(key, item)| (key.clone(), substitute_json(item, placeholder, with)))
                .collect(),
        ),
        other => other.clone(),
    }
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
    add: Vec<String>,
    remove: Vec<String>,
    env_arg: Option<Vec<String>>,
    env_forward: Option<EnvForward>,
    bridge_probe: Option<Vec<String>>,
    files: Option<FilesSpec>,
    status: StatusSpec,
    toml_set: Option<TomlSetSpec>,
    json_entry: Option<JsonEntrySpec>,
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

    /// Whether this row owns settings-file entries of its own (a `toml_set`
    /// with `entries`, or a `json_entry`): what a launch of the profile
    /// depends on for its CLI to run yagura's hooks.
    #[must_use]
    pub fn owns_entries(&self) -> bool {
        self.toml_set.as_ref().is_some_and(TomlSetSpec::has_entries) || self.json_entry.is_some()
    }

    /// Whether this row converts `{bridge}` through an environment-specific
    /// tool ([`McpRegistrationSpec::render_probe`]) rather than using the
    /// bridge path directly.
    #[must_use]
    pub fn has_bridge_probe(&self) -> bool {
        self.bridge_probe.is_some()
    }

    /// Renders the command that registers `target` with `bridge`.
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
        let env_args = self.render_env_args(target)?;
        let mut argv = Vec::with_capacity(self.add.len());
        for element in &self.add {
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

    /// Renders the command that unregisters `target`.
    #[must_use]
    pub fn render_remove(&self, target: &RegistrationTarget) -> Vec<String> {
        substitute_all(&self.remove, "{server}", target.server())
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

    /// Renders the command that turns `bridge` into this environment's own
    /// path to it, `None` when the row has no such tool (it uses `bridge`
    /// directly).
    #[must_use]
    pub fn render_probe(&self, bridge: &str) -> Option<Vec<String>> {
        self.bridge_probe
            .as_ref()
            .map(|args| substitute_all(args, "{bridge}", bridge))
    }

    /// Renders the command that reads `file`'s content, `None` when the row
    /// reads settings files directly rather than through a tool.
    #[must_use]
    pub fn render_file_read(&self, file: &str) -> Option<Vec<String>> {
        self.files
            .as_ref()
            .map(|files| substitute_all(&files.read, "{file}", file))
    }

    /// Renders the command that replaces `file`'s content (the new content
    /// is passed on its standard input), `None` when the row has no such
    /// tool.
    #[must_use]
    pub fn render_file_replace(&self, file: &str) -> Option<Vec<String>> {
        self.files
            .as_ref()
            .and_then(|files| files.replace.as_ref())
            .map(|args| substitute_all(args, "{file}", file))
    }

    /// The exit code the row's `files.read` tool uses for "the file is not
    /// there", when it declares one.
    #[must_use]
    pub fn files_absent_exit(&self) -> Option<i32> {
        self.files.as_ref().and_then(|files| files.absent_exit)
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
/// `expected_bridge` when the row has no `bridge_probe` (Windows compares
/// case-insensitively and normalizes `/` to `\`; every other host compares
/// exactly). A row with `bridge_probe` always compares exactly, on every
/// host: its probe already rendered the path the way that environment
/// itself sees it.
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
    let has_probe = spec.bridge_probe.is_some();
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

/// Validates a `bridge_probe` command's standard output as a usable path:
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
    add: Vec<String>,
    remove: Vec<String>,
    #[serde(default)]
    env_arg: Option<Vec<String>>,
    #[serde(default)]
    bridge_probe: Option<Vec<String>>,
    #[serde(default)]
    env_forward: Option<RawEnvForward>,
    #[serde(default)]
    files: Option<RawFiles>,
    status: RawStatusTable,
    #[serde(default)]
    toml_set: Option<RawTomlSet>,
    #[serde(default)]
    json_entry: Option<RawJsonEntry>,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJsonEntry {
    file: String,
    entries: toml::Table,
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
/// untyped) into a [`McpRegistrationSpec`]. `None` in, `None` out.
///
/// # Errors
///
/// Returns [`McpRegistrationError`] for a table that fails to decode (an
/// unknown or missing key, a value of the wrong shape) or breaks a rule this
/// module enforces itself (see [`McpRegistrationError::InvalidShape`]).
pub(crate) fn parse_mcp_registration(
    raw: Option<toml::Value>,
) -> Result<Option<McpRegistrationSpec>, McpRegistrationError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let table: RawMcpRegistrationTable = raw.try_into()?;
    build_spec(table).map(Some)
}

fn build_spec(raw: RawMcpRegistrationTable) -> Result<McpRegistrationSpec, McpRegistrationError> {
    let invalid = || McpRegistrationError::InvalidShape;

    if let Some(group) = &raw.group {
        yagura_protocol::validate_display_label(group).map_err(|_| invalid())?;
    }

    validate_add_placeholders(&raw.add)?;
    validate_args_placeholders(&raw.remove, &["{server}"])?;
    if let Some(env_arg) = &raw.env_arg {
        validate_args_placeholders(env_arg, &["{env}"])?;
    }
    if let Some(bridge_probe) = &raw.bridge_probe {
        validate_args_placeholders(bridge_probe, &["{bridge}"])?;
    }

    let files = match raw.files {
        Some(raw_files) => {
            validate_args_placeholders(&raw_files.read, &["{file}"])?;
            if let Some(replace) = &raw_files.replace {
                validate_args_placeholders(replace, &["{file}"])?;
            }
            Some(FilesSpec {
                read: raw_files.read,
                replace: raw_files.replace,
                absent_exit: raw_files.absent_exit,
            })
        }
        None => None,
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

    Ok(McpRegistrationSpec {
        group: raw.group,
        add: raw.add,
        remove: raw.remove,
        env_arg: raw.env_arg,
        env_forward: raw.env_forward.map(|forward| EnvForward {
            name: forward.name,
            separator: forward.separator,
        }),
        bridge_probe: raw.bridge_probe,
        files,
        status,
        toml_set,
        json_entry,
    })
}

/// Validates `json_entry`: a safe home-relative `file`, at least one entry,
/// names carrying no placeholder but `{server}`, and values carrying none
/// but `{daemon}`.
fn build_json_entry(raw: RawJsonEntry) -> Result<JsonEntrySpec, McpRegistrationError> {
    let invalid = || McpRegistrationError::InvalidShape;
    if !is_safe_relative_home_path(&raw.file) || raw.entries.is_empty() {
        return Err(invalid());
    }
    let mut entries = Vec::with_capacity(raw.entries.len());
    for (name, value) in raw.entries {
        let value = serde_json::to_value(value).map_err(|_| invalid())?;
        if profile::contains_placeholder_outside(&name, &["{server}"])
            || !json_placeholders_allowed(&value, &["{daemon}"])
        {
            return Err(invalid());
        }
        entries.push((name, value));
    }
    Ok(JsonEntrySpec {
        file: raw.file,
        entries,
    })
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
fn is_safe_relative_home_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.starts_with('\\') || path.contains(':') {
        return false;
    }
    path.split(['/', '\\']).all(|segment| segment != "..")
}
