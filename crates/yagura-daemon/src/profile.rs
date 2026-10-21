//! Launch profiles: reading, validating, enumerating and turning a profile
//! into the content of a window launch.
//!
//! A profile is one TOML file under [`crate::DaemonConfig::profiles_dir`]
//! whose name stem is its identifier. This module is the single place that
//! owns that responsibility; the IPC layer, the MCP bridge and the GUI only
//! display and forward what it returns.
//!
//! Two failure modes are kept apart throughout: a file that is *excluded*
//! from the listing entirely (an invalid identifier, an unsupported schema,
//! a missing or rule-breaking display name, an argv element that is empty or
//! contains a NUL byte, a `[model]` / `[effort]` / `[mcp]` table breaking its
//! rule), and one that stays usable while one value inside it
//! is *ignored* with a warning
//! (a relative or `~` working directory, a reserved `YAGURA_` environment
//! key). Only the first kind can make [`parse_profile`] fail.
//!
//! Both [`list_profiles`] and [`resolve_launch`] read the profiles
//! directory fresh on every call: nothing here is
//! cached, so an operator can add, edit or remove a profile without
//! restarting the daemon. What *is* fixed at start-up, and lives on
//! [`crate::DaemonConfig`] instead, is the resolution environment
//! ([`crate::DaemonConfig::program_lookup`]) and the default shell
//! ([`crate::DaemonConfig::default_shell`]).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use thiserror::Error;
use yagura_os::program;
use yagura_protocol::{
    LaunchOptionAlias, LaunchOptionInfo, ProfileGuidance, ProfileUnavailableKind,
};

use crate::child_env::is_reserved_env_name;
use crate::config::DaemonConfig;
use crate::detector::definition::is_valid_detection_name;
use crate::detector::layers::{DetectionLayers, LayerError};
use crate::host_paths::{HostPaths, HostPathsError, RawHostPaths, parse_host_paths};
use crate::profile_delivery::{DeliveryError, ProfileDelivery, WakeMode, parse_delivery};
use crate::window::{
    CliInjection, InjectedConfigDir, InjectedConfigFile, InjectionKind, RequestedCwd,
};

/// Schema version this loader accepts. Any other declared value is
/// rejected, so a future incompatible schema change fails loudly rather
/// than being read optimistically.
const SUPPORTED_SCHEMA: u32 = 1;

/// The placeholder a launch option's `args` spell where the resolved value
/// goes. Every occurrence inside an element is replaced; nothing else is.
const VALUE_PLACEHOLDER: &str = "{value}";

/// The placeholders an `[mcp]` table's `args` may spell, understood by
/// [`render_mcp_args`]. Named here so [`contains_unknown_placeholder`] strips
/// exactly these before judging what remains.
const MCP_CONFIG_FILE_PLACEHOLDER: &str = "{mcp_config_file}";
const MCP_CONFIG_DIR_PLACEHOLDER: &str = "{mcp_config_dir}";
const MCP_BRIDGE_TOML_PLACEHOLDER: &str = "{bridge_toml}";
const MCP_ENV_NAMES_TOML_PLACEHOLDER: &str = "{env_names_toml}";
const MCP_WSLENV_TOML_PLACEHOLDER: &str = "{wslenv_toml}";
/// The one placeholder an `[mcp]` table's `config_file` content may spell as
/// well as its `args`.
const MCP_SERVER_PLACEHOLDER: &str = "{server}";
/// The placeholders a `[hooks]` table's `args` may spell.
const HOOKS_CONFIG_FILE_PLACEHOLDER: &str = "{hooks_config_file}";
const HOOKS_CONFIG_DIR_PLACEHOLDER: &str = "{hooks_config_dir}";

/// Reason text for [`ProfileUnavailableKind::NotFound`]: the selected argv's
/// executable could not be found by the injected resolution environment.
const REASON_NOT_FOUND: &str = "the executable for this profile could not be found";
/// Reason text for [`ProfileUnavailableKind::Metacharacter`]: the command
/// interpreter would reinterpret part of the command line (a `.cmd` / `.bat`
/// shim path or a trailing argument carrying one of its metacharacters).
const REASON_METACHARACTER: &str =
    "the command line contains a character the command interpreter would reinterpret";
/// Reason text for [`ProfileUnavailableKind::NoCommand`]: the profile's
/// `[command]` table has neither this operating system's key nor `default`.
const REASON_NO_ARGV: &str = "no command is defined for this operating system";
/// Reason text for [`ProfileUnavailableKind::NoDefaultShell`]: the profile
/// omits `[command]` (meaning "the daemon's default shell"), but no default
/// shell was resolved at start-up.
const REASON_NO_DEFAULT_SHELL: &str = "no default shell was resolved when the daemon started";
/// Reason text for [`ProfileUnavailableKind::NeedsNativeProgram`]: the profile
/// wakes a waiting CLI by running its own program, which resolved to a Windows
/// `.cmd` / `.bat` file, and the command interpreter would reinterpret the
/// text that is passed to it. The profile's own `shim_guidance` follows this
/// sentence.
const REASON_WAKE_NEEDS_NATIVE_PROGRAM: &str =
    "this profile cannot be used through a Windows .cmd / .bat file";
/// The refusal text for a profile its `enabled` key switches off: the
/// `Display` of [`ProfileLaunchError::Disabled`] and the reason
/// [`resolve_model_choices`] gives, so both read the same.
const REASON_DISABLED: &str = "the launch profile is disabled";

/// The operating system a profile's `[command]` table distinguishes,
/// re-exported under this crate's long-standing name so the existing
/// `ProfileOs::...` call sites here need no rewrite.
///
/// The type itself, and the single place that reads `cfg!(windows)` /
/// `cfg!(target_os = ...)` to decide the host, live in
/// [`yagura_os::program::HostOs`]: the daemon does not derive the host
/// operating system on its own.
pub use program::HostOs as ProfileOs;

/// A profile's `[command]` table: up to four argv candidates, selected by
/// [`argv_for_os`].
#[derive(Debug, Clone)]
pub struct ProfileCommand {
    /// Used when the current operating system has no key of its own.
    pub default: Option<Vec<String>>,
    /// Used on [`ProfileOs::Windows`], ahead of `default`.
    pub windows: Option<Vec<String>>,
    /// Used on [`ProfileOs::MacOs`], ahead of `default`.
    pub macos: Option<Vec<String>>,
    /// Used on [`ProfileOs::Linux`], ahead of `default`.
    pub linux: Option<Vec<String>>,
}

/// The two launch options a profile can map. Named here only: what each
/// axis means to a CLI lives in the profile's TOML, never in this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchOptionAxis {
    /// The `model` option.
    Model,
    /// The `effort` option.
    Effort,
}

impl LaunchOptionAxis {
    /// Every axis, in the order [`apply_launch_options`] appends their argv
    /// elements and names the first unmapped one in a refusal.
    pub const ALL: [Self; 2] = [Self::Model, Self::Effort];

    /// The axis name as a caller spells it on the wire and as a profile
    /// names its table.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Effort => "effort",
        }
    }
}

/// One axis's mapping as a profile declares it.
#[derive(Debug, Clone)]
pub struct LaunchOptionSpec {
    /// Argv elements appended for a launch; each `{value}` is replaced.
    pub args: Vec<String>,
    /// Alias -> vendor-specific value, in the profile file's order.
    pub aliases: Vec<(String, String)>,
}

/// The per-axis specs a profile carries (`None` = axis not accepted).
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    /// The `[model]` table, if the profile declares one.
    pub model: Option<LaunchOptionSpec>,
    /// The `[effort]` table, if the profile declares one.
    pub effort: Option<LaunchOptionSpec>,
    /// The `[model.choices]` table: where the values a caller may pick for
    /// the `model` axis come from. `None` when the profile declares none.
    pub model_choices: Option<ModelChoicesSpec>,
}

/// Where a profile's model choices come from, as its `[model.choices]` table
/// declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelChoicesSpec {
    /// A fixed list, in the author's order (`values`).
    Values(Vec<String>),
    /// A command that lists them: the profile's own launch argv followed by
    /// `args`, whose standard output is read as `format` says.
    Command {
        /// Argv elements appended to the launch argv, passed as written.
        args: Vec<String>,
        /// How the command's standard output is read.
        format: OutputFormat,
    },
}

/// How a listing command's standard output is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputFormat {
    /// One choice per line: the text before the first `separator` is the
    /// identifier, the text after it (if any) the display name.
    Lines {
        /// The non-empty field separator.
        separator: String,
    },
    /// A JSON document, read through JSON Pointers (RFC 6901).
    Json {
        /// Points at the array holding one element per choice.
        items: String,
        /// Points, inside one element, at its identifier.
        id: String,
        /// Points, inside one element, at its display name, if read at all.
        label: Option<String>,
        /// Keeps only the elements whose value at a pointer equals a string.
        filter: Option<JsonFilter>,
    },
}

/// Keeps only the JSON elements whose value at `pointer` is the string
/// `equals`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonFilter {
    /// Points, inside one element, at the value compared.
    pub pointer: String,
    /// The non-empty string that value must equal.
    pub equals: String,
}

impl LaunchOptions {
    /// The spec for `axis`, `None` when the profile does not map it.
    #[must_use]
    pub fn get(&self, axis: LaunchOptionAxis) -> Option<&LaunchOptionSpec> {
        match axis {
            LaunchOptionAxis::Model => self.model.as_ref(),
            LaunchOptionAxis::Effort => self.effort.as_ref(),
        }
    }
}

/// The values a caller supplied (`None` = omitted; the CLI's own default
/// applies).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchOptionValues {
    /// The caller's `model`, if supplied.
    pub model: Option<String>,
    /// The caller's `effort`, if supplied.
    pub effort: Option<String>,
}

impl LaunchOptionValues {
    /// The value supplied for `axis`, `None` when the caller omitted it.
    #[must_use]
    pub fn get(&self, axis: LaunchOptionAxis) -> Option<&str> {
        match axis {
            LaunchOptionAxis::Model => self.model.as_deref(),
            LaunchOptionAxis::Effort => self.effort.as_deref(),
        }
    }

    /// Whether the caller supplied no value for any axis.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        LaunchOptionAxis::ALL
            .into_iter()
            .all(|axis| self.get(axis).is_none())
    }
}

/// A parsed and validated launch profile.
#[derive(Debug, Clone)]
pub struct Profile {
    /// The identifier: the source TOML file's name stem.
    pub id: String,
    /// The declared display name. Already validated against the window-name
    /// rule ([`yagura_protocol::validate_window_name`]).
    pub display_name: String,
    /// The detection definition this profile names, if any. `None` selects
    /// the daemon's configured default.
    pub detection: Option<String>,
    /// The profile's own default working directory. `None` when the
    /// document omitted `cwd`, or when it named a relative or `~` path
    /// (dropped with a warning rather than resolved against anything).
    pub cwd: Option<PathBuf>,
    /// The `[command]` table. `None` means the table was omitted entirely,
    /// which selects the daemon's default shell -- a state distinct from a
    /// `[command]` table present but naming no argv for this operating
    /// system ([`argv_for_os`] returning `None`).
    pub command: Option<ProfileCommand>,
    /// The top-level `enabled` key, `true` when the document omits it. A
    /// disabled profile stays listed but is refused when something asks to
    /// start it ([`resolve_launch`], [`resolve_model_choices`]).
    pub enabled: bool,
    /// The `[env]` table, taken verbatim (no expansion), with any
    /// `YAGURA_`-prefixed key already dropped.
    pub env: BTreeMap<String, String>,
    /// The `[model]` / `[effort]` tables, already validated. An axis the
    /// document has no table for is `None`: the profile accepts no value
    /// for it.
    pub options: LaunchOptions,
    /// The `[mcp]` table, already validated. `None` means the document has
    /// no such table at all -- a window opened from this profile carries no
    /// MCP-bridge injection.
    pub mcp: Option<InjectionSpec>,
    /// The `[hooks]` table, already validated. `None` means the document has
    /// no such table at all -- a window opened from this profile carries no
    /// status-report hooks.
    pub hooks: Option<InjectionSpec>,
    /// The top-level `[mcp_registration]` table, already validated. `None`
    /// means the document has no such table. This never affects opening a
    /// window from this profile: it only describes how the daemon can
    /// register its own MCP bridge with the CLI outside any window.
    pub mcp_registration: Option<crate::mcp_registration::McpRegistrationSpec>,
    /// The `[delivery]` table, already validated. `None` means the document
    /// has no such table: the daemon hands this profile's windows nothing
    /// without typing it into the composer.
    pub delivery: Option<ProfileDelivery>,
    /// The top-level `[host_paths]` table, already validated: how a
    /// daemon-side path becomes the path this profile's CLI sees. `None`
    /// means paths are handed over unchanged.
    pub host_paths: Option<HostPaths>,
    /// Values that were dropped or ignored while staying valid overall
    /// (a relative/`~` `cwd`, a reserved environment key). Empty when
    /// nothing needed to be reported.
    pub warnings: Vec<String>,
}

/// One profile as [`list_profiles`] reports it.
#[derive(Debug, Clone)]
pub struct ProfileEntry {
    /// The identifier: the source TOML file's name stem.
    pub id: String,
    /// The declared display name.
    pub display_name: String,
    /// The detection definition this profile names, if any.
    pub detection: Option<String>,
    /// English reason text when this entry's detection cannot be counted
    /// on: the name breaks the naming rule, no definition layer is
    /// configured at all, the name is absent from an inventory that was
    /// read, or that inventory could not be read. `None` when the name is
    /// present in the inventory, and when `detection` is `None` unless no
    /// layer is configured at all.
    pub detection_warning: Option<String>,
    /// The profile's own default working directory, if it declared one.
    pub cwd: Option<PathBuf>,
    /// The unresolved argv a launch of this entry would carry
    /// ([`ProfileLaunch::argv`]), empty when there is none to carry: an
    /// omitted `[command]` table with no default shell resolved at start-up.
    pub command: Vec<String>,
    /// The profile's `enabled` key ([`Profile::enabled`]).
    pub enabled: bool,
    /// Whether the profile currently resolves to something launchable.
    /// Exactly the absence of [`ProfileEntry::unavailable_reason`]
    /// (asserted by the daemon's own tests): `available` never disagrees
    /// with the reason field beside it.
    pub available: bool,
    /// English reason text when `available` is `false`.
    pub unavailable_reason: Option<String>,
    /// The kind of [`ProfileEntry::unavailable_reason`], decided with it;
    /// `None` exactly when that reason is.
    pub unavailable_kind: Option<ProfileUnavailableKind>,
    /// The profile's own guidance that follows the reason text, carried
    /// apart from it ([`Unavailability::guidance`]).
    pub unavailable_guidance: Option<ProfileGuidance>,
    /// The `model` option this profile maps, `None` when it accepts none.
    pub model: Option<LaunchOptionInfo>,
    /// The `effort` option this profile maps, `None` when it accepts none.
    pub effort: Option<LaunchOptionInfo>,
}

/// The result of enumerating the resolved profiles directory.
#[derive(Debug, Clone)]
pub struct ProfileListing {
    /// Every usable profile found.
    pub profiles: Vec<ProfileEntry>,
    /// One line per excluded file, plus -- when nothing could be
    /// enumerated at all -- one line explaining why. Each line names the
    /// file and a fixed reason only ([`ProfileParseError`]): these lines
    /// reach clients, while the underlying detail goes to the daemon's log.
    pub warnings: Vec<String>,
}

/// The content of a window launch derived from a profile, ready to be
/// handed to [`crate::window::WindowRegistry::open_with_extra_env`].
#[derive(Debug, Clone)]
pub struct ProfileLaunch {
    /// The argv the caller wrote, *unresolved*: resolving `argv[0]` to a
    /// concrete executable happens immediately before spawn,
    /// not here.
    ///
    /// Order contract: the profile's own argv first, then the elements the
    /// supplied launch options map to, axis by axis in
    /// [`LaunchOptionAxis::ALL`] order (`model` before `effort`), each axis's
    /// elements in its `args` order. A restored window replays this argv, so
    /// the order is fixed rather than left to the caller.
    pub argv: Vec<String>,
    /// The resolved working directory, with where it came from: the
    /// caller's request, or the profile's own default.
    pub cwd: Option<RequestedCwd>,
    /// The resolved display name: the caller's request, or the profile's
    /// `display_name`.
    pub name: String,
    /// The vendor badge: the profile identifier.
    pub vendor: String,
    /// The profile's detection value, verbatim.
    pub detection: Option<String>,
    /// The profile's environment, with reserved keys already dropped.
    pub env: BTreeMap<String, String>,
    /// The profile's `[mcp]` table, unrendered. `None` when the profile has
    /// none (see [`prepare_mcp_injection`] for rendering it).
    pub mcp: Option<InjectionSpec>,
    /// The profile's `[hooks]` table, unrendered. `None` when the profile has
    /// none (see [`prepare_hooks_injection`] for rendering it).
    pub hooks: Option<InjectionSpec>,
    /// The profile's registration table, resolved alongside `hooks`: the settings
    /// file entries it owns decide whether a launch may inject them, or --
    /// for a `json_entry`, which writes the CLI's hooks into its own settings
    /// -- whether the CLI will report. `None` when the profile has neither
    /// `[hooks]` nor a `json_entry`, or has no registration table.
    pub hooks_registration: Option<crate::mcp_registration::McpRegistrationSpec>,
    /// The profile's `[mcp_registration.list]` table: how this launch asks
    /// its CLI for the bridge registrations of other instances and turns
    /// them off. `None` when the profile declares none.
    pub bridge_listing: Option<crate::mcp_registration::ListSpec>,
    /// The profile's `[host_paths]` table: how the daemon-side paths an
    /// injection names are converted before they reach the argv. `None` when
    /// the profile declares none.
    pub host_paths: Option<HostPaths>,
    /// The profile's `[delivery]` table, as declared. What it contributes to a
    /// spawn argv (a launch prompt) is rendered by
    /// [`crate::profile_delivery::launch_prompt_injection`] and never enters
    /// [`ProfileLaunch::argv`].
    pub delivery: Option<ProfileDelivery>,
}

/// A profile's `[mcp]` or `[hooks]` table: argv elements (and the settings
/// file they may name) appended to the CLI a launched window spawns. `[mcp]`
/// points the CLI at the daemon's own MCP bridge; `[hooks]` makes it report
/// its turn boundaries. `None` on [`Profile::mcp`] / [`Profile::hooks`]
/// (rather than an instance of this type) means the profile has no such
/// table at all -- the vendor-neutral way of saying its CLI has no way to
/// receive that configuration at launch.
#[derive(Debug, Clone)]
pub struct InjectionSpec {
    /// Argv elements to append at spawn. The placeholders of the table's
    /// kind ([`render_mcp_args`] for `[mcp]`, [`render_hooks_args`] for
    /// `[hooks]`) are substituted there; an element with none of them passes
    /// through unchanged.
    pub args: Vec<String>,
    /// The settings-file content the daemon writes to disk before spawn,
    /// substituted wherever an `args` element spells the kind's config-file
    /// placeholder. For `[mcp]`, `{server}` in it is substituted too, and
    /// nothing else. `None` when no element needs one (the CLI takes the
    /// configuration directly on its command line instead).
    pub config_file: Option<String>,
    /// The files of a settings directory the daemon writes before spawn,
    /// file name to content, substituted wherever an `args` element spells
    /// the kind's config-directory placeholder: for a CLI that takes its
    /// configuration only as a directory. Each name is one path component
    /// ([`is_config_dir_file_name`]); `{server}` in an `[mcp]` table's
    /// contents is substituted, and nothing else. `None` when the table has
    /// none.
    pub config_dir: Option<BTreeMap<String, String>>,
}

impl InjectionSpec {
    /// Whether the table names its MCP server through `{server}`, so the
    /// name a launch injects is the one the caller renders it with. A table
    /// that spells the name out injects one this crate cannot read back.
    #[must_use]
    pub fn names_server(&self) -> bool {
        self.args
            .iter()
            .chain(self.config_file.as_ref())
            .chain(self.config_dir.iter().flat_map(BTreeMap::values))
            .any(|text| text.contains(MCP_SERVER_PLACEHOLDER))
    }
}

/// Where the settings an injection names were placed for one launch: what
/// replaces the kind's config-file and config-directory placeholders.
#[derive(Debug, Clone, Copy, Default)]
pub struct InjectedPaths<'a> {
    /// The settings file's path, when the table has a `config_file`.
    pub config_file: Option<&'a Path>,
    /// The settings directory's path, when the table has a `config_dir`.
    pub config_dir: Option<&'a Path>,
}

/// Errors rejecting a profile document outright (the *excluded* half of the
/// two failure modes this module distinguishes; see the module doc).
///
/// Every variant's `Display` is a fixed phrase that names nothing read from
/// the document: [`list_profiles`] puts it in a client-visible warning, and a
/// profile's own text -- a TOML syntax error's offending line, above all --
/// can hold its author's secret. The underlying detail stays reachable
/// through [`std::error::Error::source`], which only the daemon's log reads.
#[derive(Debug, Clone, Error)]
pub enum ProfileParseError {
    /// The identifier (the file name stem) does not match
    /// `[a-z0-9_-]{1,64}`.
    #[error("invalid identifier")]
    InvalidIdentifier,
    /// The document failed to parse as TOML, or is missing a required key
    /// (`schema`, `display_name`).
    #[error("could not be parsed")]
    Toml(#[from] toml::de::Error),
    /// `schema` is not [`SUPPORTED_SCHEMA`].
    #[error("unsupported schema")]
    UnsupportedSchema(#[source] UnsupportedSchemaVersion),
    /// `display_name` violates the window-label rule.
    #[error("invalid display name")]
    InvalidDisplayName(#[source] yagura_protocol::WindowNameError),
    /// An argv under `[command]`, or a launch option's `args`, contains an
    /// empty element or a NUL byte.
    #[error("invalid argv element")]
    InvalidArgvElement(#[from] yagura_protocol::ArgvElementError),
    /// A `[model]` / `[effort]` table breaks a launch option rule
    /// ([`LaunchOptionError`]).
    #[error("invalid launch option")]
    InvalidLaunchOption(#[source] LaunchOptionError),
    /// A `[mcp]` table breaks an injection rule ([`InjectionSpecError`]).
    #[error("invalid mcp table")]
    InvalidMcp(#[source] InjectionSpecError),
    /// A `[hooks]` table breaks an injection rule ([`InjectionSpecError`]).
    #[error("invalid hooks table")]
    InvalidHooks(#[source] InjectionSpecError),
    /// A `[delivery]` table breaks its rule ([`DeliveryError`]).
    #[error("invalid delivery table")]
    InvalidDelivery(#[source] DeliveryError),
    /// A `[mcp_registration]` table breaks its rule
    /// ([`crate::mcp_registration::McpRegistrationError`]). A valid table
    /// never changes how a window launches, but a broken one still excludes
    /// the whole profile, the same rule every other table follows.
    #[error("invalid mcp registration table")]
    InvalidMcpRegistration(#[source] crate::mcp_registration::McpRegistrationError),
    /// A `[host_paths]` table breaks its rule ([`HostPathsError`]).
    #[error("invalid host paths table")]
    InvalidHostPaths(#[source] HostPathsError),
}

/// Why a `[model]` / `[effort]` table was rejected. Carried only as the
/// `source()` of [`ProfileParseError::InvalidLaunchOption`], so it reaches
/// the daemon's log without appearing in the fixed client-visible warning.
#[derive(Debug, Clone, Error)]
pub enum LaunchOptionError {
    /// The table has no `args` key.
    #[error("the table has no `args`")]
    MissingArgs,
    /// No element of `args` contains the `{value}` placeholder.
    #[error("`args` does not contain the {{value}} placeholder")]
    MissingPlaceholder,
    /// An alias key is not a valid identifier ([`validate_profile_id`]).
    #[error("an alias key is not a valid identifier")]
    AliasKey,
    /// An alias value violates the launch option value rule
    /// ([`yagura_protocol::validate_launch_option_value`]).
    #[error("an alias value violates the launch option value rule")]
    AliasValue(#[source] yagura_protocol::LaunchOptionValueError),
    /// The `[model.choices]` table breaks its rule: not exactly one of
    /// `values` / `args`, an empty or duplicated list, a value outside the
    /// launch option value rule, a missing or unknown `format`, a key the
    /// format does not take, or a pointer outside the JSON Pointer syntax.
    #[error("the choices table breaks its rule")]
    InvalidChoices,
}

/// Why a `[mcp]` or `[hooks]` table was rejected (both follow the same
/// rules). Carried only as the `source()` of
/// [`ProfileParseError::InvalidMcp`] / [`ProfileParseError::InvalidHooks`],
/// mirroring [`LaunchOptionError`].
#[derive(Debug, Clone, Error)]
pub enum InjectionSpecError {
    /// The table has no `args` key.
    #[error("the table has no `args`")]
    MissingArgs,
    /// An `args` element carries a `{name}`-shaped placeholder the table's
    /// kind does not understand.
    #[error("args carries a placeholder the table does not understand")]
    UnknownPlaceholder,
    /// An `args` element uses the kind's config-file placeholder, but the
    /// table has no `config_file` key to substitute there.
    #[error("args uses the config file placeholder but the table has no config_file")]
    ConfigFileRequiredForPlaceholder,
    /// An `args` element uses the kind's config-directory placeholder, but
    /// the table has no `config_dir` table to write there.
    #[error("args uses the config directory placeholder but the table has no config_dir")]
    ConfigDirRequiredForPlaceholder,
    /// The `config_dir` table names no file.
    #[error("the config_dir table is empty")]
    EmptyConfigDir,
    /// A `config_dir` key is not a single plain file name
    /// ([`is_config_dir_file_name`]).
    #[error("a config_dir key is not a plain file name")]
    InvalidConfigDirFileName,
}

/// The schema version [`ProfileParseError::UnsupportedSchema`] rejected.
/// Carried only as that variant's [`std::error::Error::source`], so it
/// reaches the daemon's log without appearing in the fixed client-visible
/// warning.
#[derive(Debug, Clone, Error)]
#[error("schema version {0}")]
pub struct UnsupportedSchemaVersion(pub u32);

/// Errors refusing to resolve a launch (`OpenWindowFromProfile`'s
/// `invalid_argument` conditions).
#[derive(Debug, Clone, Error)]
pub enum ProfileLaunchError {
    /// The identifier is not one [`list_profiles`] currently reports (an
    /// invalid identifier, a file that does not exist, one that fails
    /// [`parse_profile`], or one whose `[command]` names no argv for the
    /// current operating system).
    #[error("unknown launch profile")]
    UnknownProfile,
    /// The profile's `enabled` key switches it off.
    #[error("{REASON_DISABLED}")]
    Disabled,
    /// The profile has a `[command]` table, but it names no argv for the
    /// current operating system. [`resolve_launch`] never returns it: such a
    /// profile is not listed, so it is [`ProfileLaunchError::UnknownProfile`].
    #[error("{REASON_NO_ARGV}")]
    NoArgvForOperatingSystem,
    /// The profile omits `[command]` (meaning "the default shell"), but no
    /// default shell was resolved at start-up.
    #[error("{REASON_NO_DEFAULT_SHELL}")]
    NoDefaultShell,
    /// The caller supplied a value for an axis the profile has no table for.
    /// Refused before anything is spawned: launching with the value dropped
    /// would start the wrong thing silently.
    #[error("this profile does not map the {} option", .0.name())]
    UnsupportedLaunchOption(LaunchOptionAxis),
}

/// What [`resolve_model_choices`] found for a profile's model choices.
// Built once per request and consumed at once, so its size costs nothing,
// while boxing would put an indirection in every caller's pattern.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelChoicesPlan {
    /// The profile maps the model axis but declares no `[model.choices]`.
    NoChoices,
    /// The profile's fixed list, in its author's order.
    Values(Vec<String>),
    /// The listing command to run once, outside any window.
    Command {
        /// The unresolved launch argv followed by the choices `args`: what
        /// the command is shown as, and whose `argv[0]` names a program that
        /// failed to start.
        argv: Vec<String>,
        /// `argv` resolved the same way a window's argv is: what is actually
        /// started.
        spawn_argv: Vec<String>,
        /// The unresolved launch argv alone, before the choices `args`.
        launch_argv: Vec<String>,
        /// The profile's `[env]`, reserved keys already dropped. Nothing a
        /// window receives from the daemon is added: this is not a window.
        env: Vec<(String, String)>,
        /// The profile's own working directory, if it declared one.
        cwd: Option<PathBuf>,
        /// How the command's standard output is read.
        format: OutputFormat,
    },
}

/// Why [`resolve_model_choices`] produced no plan. Every `Display` is a fixed
/// phrase naming no path.
#[derive(Debug, Clone, Error)]
pub enum ModelChoicesError {
    /// The identifier is not one [`list_profiles`] currently reports.
    #[error("unknown launch profile")]
    UnknownProfile,
    /// The profile has no `[model]` table.
    #[error("this profile does not map the model option")]
    NoModelOption,
    /// The listing command cannot be started: the reason text
    /// [`unavailable_reason`] gives, or the metacharacter reason when the
    /// choices `args` would be reinterpreted by the command interpreter.
    #[error("{0}")]
    Unavailable(String),
}

/// Validates a launch profile identifier: the TOML file's name stem.
///
/// Delegates to [`yagura_protocol::is_valid_token`], the single
/// implementation of the shared `[a-z0-9_-]{1,64}` character-set rule.
/// Because the rule already excludes every path
/// separator and `.`, an identifier that passes this check can never be
/// used to build a path outside the profiles directory.
#[must_use]
pub fn validate_profile_id(id: &str) -> bool {
    yagura_protocol::is_valid_token(id)
}

/// Raw, pre-validation representation of a profile document. Structural
/// rejection (a missing `schema` or `display_name`, or a `[command]`/`[env]`
/// table of the wrong shape) happens here, through `serde`/`toml`, and
/// surfaces as [`ProfileParseError::Toml`].
#[derive(Debug, Deserialize)]
struct RawProfile {
    schema: u32,
    display_name: String,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    detection: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    command: Option<RawCommand>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    model: Option<RawModelOption>,
    #[serde(default)]
    effort: Option<RawLaunchOption>,
    #[serde(default)]
    mcp: Option<RawInjection>,
    #[serde(default)]
    hooks: Option<RawInjection>,
    /// Kept untyped here so a broken table is classified as
    /// [`ProfileParseError::InvalidMcpRegistration`] -- with its own
    /// document-independent `Display` -- rather than
    /// [`ProfileParseError::Toml`], which [`RawMcp`]'s `deny_unknown_fields`
    /// would produce if this were decoded directly into a typed struct here.
    #[serde(default)]
    mcp_registration: Option<toml::Value>,
    /// Untyped for the same reason as `mcp_registration`: every rule it breaks
    /// is [`ProfileParseError::InvalidDelivery`] with a document-independent
    /// `Display`.
    #[serde(default)]
    delivery: Option<toml::Value>,
    #[serde(default)]
    host_paths: Option<RawHostPaths>,
}

/// A `[mcp]` or `[hooks]` table before validation. `args` is `Option` for the
/// same reason [`RawLaunchOption::args`] is: its absence is classified as an
/// injection rule ([`InjectionSpecError::MissingArgs`]) rather than a decode
/// failure; an unknown key is refused while decoding
/// ([`ProfileParseError::Toml`]).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInjection {
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    config_file: Option<String>,
    #[serde(default)]
    config_dir: Option<BTreeMap<String, String>>,
}

/// A `[model]` / `[effort]` table before validation. `args` is `Option` so
/// its absence is classified as a launch option rule
/// ([`LaunchOptionError::MissingArgs`]) rather than a decode failure; an
/// unknown key, by contrast, is refused while decoding
/// ([`ProfileParseError::Toml`]) so a misspelt `aliases` cannot silently
/// mean "no aliases".
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLaunchOption {
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    aliases: OrderedAliases,
}

/// The `[model]` table before validation: a [`RawLaunchOption`] that may also
/// carry a `choices` table. Only the model axis has one, so `[effort]` keeps
/// refusing a `choices` key as unknown.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawModelOption {
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    aliases: OrderedAliases,
    #[serde(default)]
    choices: Option<RawModelChoices>,
}

/// A `[model.choices]` table before validation. Every key is optional here
/// so that a missing or misplaced one is classified as a choices rule
/// ([`LaunchOptionError::InvalidChoices`]) rather than a decode failure; an
/// unknown key is still refused while decoding.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawModelChoices {
    #[serde(default)]
    values: Option<Vec<String>>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    separator: Option<String>,
    #[serde(default)]
    items: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    filter: Option<RawJsonFilter>,
}

/// A `filter = { pointer, equals }` inline table before validation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJsonFilter {
    #[serde(default)]
    pointer: Option<String>,
    #[serde(default)]
    equals: Option<String>,
}

/// An alias table read in document order. `toml` hands map entries over in
/// the order the file spells them; a `Vec` keeps that order where a map type
/// would sort or hash it away, and the listing promises the author's order.
#[derive(Debug, Default)]
struct OrderedAliases(Vec<(String, String)>);

impl<'de> Deserialize<'de> for OrderedAliases {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OrderedAliasesVisitor;

        impl<'de> Visitor<'de> for OrderedAliasesVisitor {
            type Value = OrderedAliases;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a table of alias = \"value\" pairs")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut pairs = Vec::with_capacity(map.size_hint().unwrap_or(0));
                while let Some(pair) = map.next_entry::<String, String>()? {
                    pairs.push(pair);
                }
                Ok(OrderedAliases(pairs))
            }
        }

        deserializer.deserialize_map(OrderedAliasesVisitor)
    }
}

#[derive(Debug, Deserialize)]
struct RawCommand {
    #[serde(default)]
    default: Option<Vec<String>>,
    #[serde(default)]
    windows: Option<Vec<String>>,
    #[serde(default)]
    macos: Option<Vec<String>>,
    #[serde(default)]
    linux: Option<Vec<String>>,
}

/// Parses and validates one profile document.
///
/// `id` is the file name stem the caller enumerated this document under; it
/// is validated here rather than by the caller so that every entry point
/// (fixture-driven tests, [`list_profiles`], [`resolve_launch`]) applies the
/// same rule.
///
/// # Errors
///
/// Returns [`ProfileParseError`] for an identifier outside the allowed
/// character set, malformed TOML (including an unknown key inside a
/// `[model]` / `[effort]` / `[mcp]` table), an unsupported schema version, a
/// display name that violates the window-label rule, an argv element (under
/// `[command]` or in a launch option's `args`) that is empty or contains a
/// NUL byte, a launch option table that breaks its rule
/// ([`LaunchOptionError`]: no `args`, no `{value}` placeholder, an alias key
/// outside the identifier rule, an alias value outside the launch option
/// value rule, a `[model.choices]` table breaking its rule), or an `[mcp]` /
/// `[hooks]` table that breaks its rule ([`InjectionSpecError`]: no `args`, an
/// unknown placeholder, the config-file placeholder used without a
/// `config_file`). A relative or `~` working directory and a reserved
/// `YAGURA_`-prefixed environment key are *not* errors: both are dropped
/// with a warning, recorded on [`Profile::warnings`], and the profile stays
/// usable.
pub fn parse_profile(id: &str, source: &str) -> Result<Profile, ProfileParseError> {
    if !validate_profile_id(id) {
        return Err(ProfileParseError::InvalidIdentifier);
    }

    let raw: RawProfile = toml::from_str(source)?;

    if raw.schema != SUPPORTED_SCHEMA {
        return Err(ProfileParseError::UnsupportedSchema(
            UnsupportedSchemaVersion(raw.schema),
        ));
    }
    yagura_protocol::validate_window_name(&raw.display_name)
        .map_err(ProfileParseError::InvalidDisplayName)?;
    if let Some(command) = &raw.command {
        for argv in [
            &command.default,
            &command.windows,
            &command.macos,
            &command.linux,
        ]
        .into_iter()
        .flatten()
        {
            yagura_protocol::validate_argv_elements(argv)?;
        }
    }
    let (model, model_choices) = match raw.model {
        Some(RawModelOption {
            args,
            aliases,
            choices,
        }) => (
            parse_launch_option(Some(RawLaunchOption { args, aliases }))?,
            choices.map(parse_model_choices).transpose()?,
        ),
        None => (None, None),
    };
    let options = LaunchOptions {
        model,
        effort: parse_launch_option(raw.effort)?,
        model_choices,
    };
    let mcp = parse_injection_spec(raw.mcp, TableKind::Mcp)?;
    let hooks = parse_injection_spec(raw.hooks, TableKind::Hooks)?;
    let host_paths =
        parse_host_paths(raw.host_paths).map_err(ProfileParseError::InvalidHostPaths)?;
    let mcp_registration =
        crate::mcp_registration::parse_mcp_registration(raw.mcp_registration, host_paths.as_ref())
            .map_err(ProfileParseError::InvalidMcpRegistration)?;
    let delivery = parse_delivery(raw.delivery).map_err(ProfileParseError::InvalidDelivery)?;

    let mut warnings = Vec::new();

    let cwd = raw.cwd.as_deref().and_then(|raw_cwd| {
        if Path::new(raw_cwd).is_absolute() {
            Some(PathBuf::from(raw_cwd))
        } else {
            warnings.push(format!(
                "cwd {raw_cwd:?} is not an absolute path and was ignored"
            ));
            None
        }
    });

    let mut env = BTreeMap::new();
    for (key, value) in raw.env {
        if is_reserved_env_name(OsStr::new(&key)) {
            warnings.push(format!(
                "environment key {key:?} is reserved for the daemon's own use and was dropped"
            ));
            continue;
        }
        env.insert(key, value);
    }

    let command = raw.command.map(|raw_command| ProfileCommand {
        default: raw_command.default,
        windows: raw_command.windows,
        macos: raw_command.macos,
        linux: raw_command.linux,
    });

    Ok(Profile {
        id: id.to_string(),
        display_name: raw.display_name,
        detection: raw.detection,
        cwd,
        command,
        enabled: raw.enabled.unwrap_or(true),
        env,
        options,
        mcp,
        hooks,
        mcp_registration,
        delivery,
        host_paths,
        warnings,
    })
}

/// Validates one `[model]` / `[effort]` table into its spec (`None` in,
/// `None` out), in the order: `args` present, its elements under the argv
/// rule, the `{value}` placeholder present, each alias key, each alias value.
fn parse_launch_option(
    raw: Option<RawLaunchOption>,
) -> Result<Option<LaunchOptionSpec>, ProfileParseError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let invalid = ProfileParseError::InvalidLaunchOption;
    let args = raw.args.ok_or(invalid(LaunchOptionError::MissingArgs))?;
    yagura_protocol::validate_argv_elements(&args)?;
    if !args.iter().any(|arg| arg.contains(VALUE_PLACEHOLDER)) {
        return Err(invalid(LaunchOptionError::MissingPlaceholder));
    }
    let OrderedAliases(aliases) = raw.aliases;
    for (alias, value) in &aliases {
        if !yagura_protocol::is_valid_token(alias) {
            return Err(invalid(LaunchOptionError::AliasKey));
        }
        yagura_protocol::validate_launch_option_value(value)
            .map_err(|err| invalid(LaunchOptionError::AliasValue(err)))?;
    }
    Ok(Some(LaunchOptionSpec { args, aliases }))
}

/// Validates a `[model.choices]` table into its spec. Every rule it breaks
/// is reported as the one [`LaunchOptionError::InvalidChoices`].
fn parse_model_choices(raw: RawModelChoices) -> Result<ModelChoicesSpec, ProfileParseError> {
    parse_model_choices_spec(raw).ok_or(ProfileParseError::InvalidLaunchOption(
        LaunchOptionError::InvalidChoices,
    ))
}

fn parse_model_choices_spec(raw: RawModelChoices) -> Option<ModelChoicesSpec> {
    let RawModelChoices {
        values,
        args,
        format,
        separator,
        items,
        id,
        label,
        filter,
    } = raw;
    match (values, args) {
        (Some(values), None) => {
            let command_keys_absent = format.is_none()
                && separator.is_none()
                && items.is_none()
                && id.is_none()
                && label.is_none()
                && filter.is_none();
            // A picked value is stored as an alias value, so it must pass the
            // same rule one does.
            let mut seen = std::collections::BTreeSet::new();
            let values_valid = !values.is_empty()
                && values.iter().all(|value| {
                    yagura_protocol::validate_launch_option_value(value).is_ok()
                        && seen.insert(value.as_str())
                });
            (command_keys_absent && values_valid).then_some(ModelChoicesSpec::Values(values))
        }
        (None, Some(args)) => {
            if args.is_empty() || yagura_protocol::validate_argv_elements(&args).is_err() {
                return None;
            }
            let format = match format.as_deref()? {
                "lines" => {
                    let separator = separator.filter(|separator| !separator.is_empty())?;
                    let json_keys_absent =
                        items.is_none() && id.is_none() && label.is_none() && filter.is_none();
                    json_keys_absent.then_some(OutputFormat::Lines { separator })?
                }
                "json" => {
                    if separator.is_some() {
                        return None;
                    }
                    let items = items.filter(|pointer| is_json_pointer(pointer))?;
                    let id = id.filter(|pointer| is_json_pointer(pointer))?;
                    let label = match label {
                        Some(pointer) if !is_json_pointer(&pointer) => return None,
                        label => label,
                    };
                    let filter = match filter {
                        Some(filter) => Some(parse_json_filter(filter)?),
                        None => None,
                    };
                    OutputFormat::Json {
                        items,
                        id,
                        label,
                        filter,
                    }
                }
                _ => return None,
            };
            Some(ModelChoicesSpec::Command { args, format })
        }
        _ => None,
    }
}

fn parse_json_filter(raw: RawJsonFilter) -> Option<JsonFilter> {
    let pointer = raw.pointer.filter(|pointer| is_json_pointer(pointer))?;
    let equals = raw.equals.filter(|equals| !equals.is_empty())?;
    Some(JsonFilter { pointer, equals })
}

/// The JSON Pointer syntax `serde_json::Value::pointer` accepts: the empty
/// string (the whole document) or a string starting with `/`. Shared with
/// [`crate::mcp_registration`], which validates the same syntax for its own
/// `[mcp_registration.status]` pointers.
pub(crate) fn is_json_pointer(pointer: &str) -> bool {
    pointer.is_empty() || pointer.starts_with('/')
}

/// The two tables whose `args` are argv injections with a settings file
/// behind them. [`InjectionKind`] names every injection a launch carries,
/// including ones that come from other tables and have neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TableKind {
    Mcp,
    Hooks,
}

impl TableKind {
    fn injection_kind(self) -> InjectionKind {
        match self {
            Self::Mcp => InjectionKind::Mcp,
            Self::Hooks => InjectionKind::Hooks,
        }
    }
}

/// The placeholder `kind`'s `args` spell where the settings file's path goes.
fn config_file_placeholder(kind: TableKind) -> &'static str {
    match kind {
        TableKind::Mcp => MCP_CONFIG_FILE_PLACEHOLDER,
        TableKind::Hooks => HOOKS_CONFIG_FILE_PLACEHOLDER,
    }
}

/// The placeholder `kind`'s `args` spell where the settings directory's path
/// goes.
fn config_dir_placeholder(kind: TableKind) -> &'static str {
    match kind {
        TableKind::Mcp => MCP_CONFIG_DIR_PLACEHOLDER,
        TableKind::Hooks => HOOKS_CONFIG_DIR_PLACEHOLDER,
    }
}

/// Every placeholder `kind`'s `args` may spell.
fn known_placeholders(kind: TableKind) -> &'static [&'static str] {
    match kind {
        TableKind::Mcp => &[
            MCP_CONFIG_FILE_PLACEHOLDER,
            MCP_CONFIG_DIR_PLACEHOLDER,
            MCP_BRIDGE_TOML_PLACEHOLDER,
            MCP_ENV_NAMES_TOML_PLACEHOLDER,
            MCP_WSLENV_TOML_PLACEHOLDER,
            MCP_SERVER_PLACEHOLDER,
        ],
        TableKind::Hooks => &[HOOKS_CONFIG_FILE_PLACEHOLDER, HOOKS_CONFIG_DIR_PLACEHOLDER],
    }
}

/// Whether `name` can be a `config_dir` key: one plain file name that stays
/// inside the directory on every platform. Only ASCII letters, digits, `.`,
/// `_` and `-`, so no path separator can appear; no leading `.` (no hidden
/// file, no `.` / `..`), no trailing `.` (Windows strips it), no `..`
/// anywhere, and no Windows reserved device name as its stem.
#[must_use]
pub fn is_config_dir_file_name(name: &str) -> bool {
    const RESERVED: &[&str] = &["con", "prn", "aux", "nul"];
    if name.is_empty()
        || name.len() > 255
        || name.starts_with('.')
        || name.ends_with('.')
        || name.contains("..")
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    let numbered_device = (stem.starts_with("com") || stem.starts_with("lpt"))
        && stem.len() == 4
        && stem.as_bytes()[3].is_ascii_digit();
    !(RESERVED.contains(&stem.as_str()) || numbered_device)
}

/// Validates one `[mcp]` / `[hooks]` table into its spec (`None` in, `None`
/// out), in the order: `args` present, no `args` element carries a
/// placeholder `kind` does not know, the config-file placeholder used only
/// alongside a `config_file`, the config-directory placeholder used only
/// alongside a `config_dir`, and a `config_dir` naming at least one file,
/// each by a plain file name.
fn parse_injection_spec(
    raw: Option<RawInjection>,
    kind: TableKind,
) -> Result<Option<InjectionSpec>, ProfileParseError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let invalid = match kind {
        TableKind::Mcp => ProfileParseError::InvalidMcp,
        TableKind::Hooks => ProfileParseError::InvalidHooks,
    };
    let args = raw.args.ok_or(invalid(InjectionSpecError::MissingArgs))?;
    if args
        .iter()
        .any(|arg| contains_placeholder_outside(arg, known_placeholders(kind)))
    {
        return Err(invalid(InjectionSpecError::UnknownPlaceholder));
    }
    if raw.config_file.is_none()
        && args
            .iter()
            .any(|arg| arg.contains(config_file_placeholder(kind)))
    {
        return Err(invalid(
            InjectionSpecError::ConfigFileRequiredForPlaceholder,
        ));
    }
    match &raw.config_dir {
        None if args
            .iter()
            .any(|arg| arg.contains(config_dir_placeholder(kind))) =>
        {
            return Err(invalid(InjectionSpecError::ConfigDirRequiredForPlaceholder));
        }
        Some(files) if files.is_empty() => {
            return Err(invalid(InjectionSpecError::EmptyConfigDir));
        }
        Some(files) if !files.keys().all(|name| is_config_dir_file_name(name)) => {
            return Err(invalid(InjectionSpecError::InvalidConfigDirFileName));
        }
        _ => {}
    }
    Ok(Some(InjectionSpec {
        args,
        config_file: raw.config_file,
        config_dir: raw.config_dir,
    }))
}

/// Whether `arg`, with every occurrence of each placeholder in `known`
/// removed, still contains a `{name}`-shaped token (one or more ASCII
/// letters, digits or underscores between a `{` and the nearest following
/// `}`) -- meaning it names a placeholder none of `known` covers. Scans every
/// `{` in the string (not just the first), so a placeholder-shaped token
/// nested inside an unrelated JSON literal's braces is still found.
///
/// A `{` immediately preceded by `$` is skipped: `${NAME}` is a CLI's own (or
/// its config file's) environment-variable-reference syntax, not a
/// placeholder this crate renders. Shared by [`contains_unknown_placeholder`]
/// (the `[mcp]` table's placeholders) and [`crate::mcp_registration`] (the
/// `[mcp_registration]` table's, a different set per argv position).
pub(crate) fn contains_placeholder_outside(arg: &str, known: &[&str]) -> bool {
    let mut stripped = arg.to_string();
    for placeholder in known {
        stripped = stripped.replace(placeholder, "");
    }
    let bytes = stripped.as_bytes();
    for (open, _) in stripped.match_indices('{') {
        if open > 0 && bytes[open - 1] == b'$' {
            continue;
        }
        let after = &stripped[open + 1..];
        let Some(close) = after.find('}') else {
            continue;
        };
        let name = &after[..close];
        if !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return true;
        }
    }
    false
}

/// Wraps `text` as a TOML literal string (`'...'`): no backslash escaping
/// happens inside one, so a Windows path's backslashes survive unchanged and
/// the rendered element never carries a `"`. Returns `None` when `text`
/// contains a single quote or a newline -- the only two things TOML's
/// literal-string grammar cannot represent -- so a caller refuses rather
/// than emit a broken value.
fn toml_literal_string(text: &str) -> Option<String> {
    if text.contains('\'') || text.contains('\n') {
        return None;
    }
    Some(format!("'{text}'"))
}

/// Wraps `items` as a TOML literal-string array (`['a', 'b']`), sharing
/// [`toml_literal_string`]'s per-element quoting and its refusal rule.
fn toml_literal_string_array(items: &[&str]) -> Option<String> {
    let mut quoted = Vec::with_capacity(items.len());
    for item in items {
        quoted.push(toml_literal_string(item)?);
    }
    Some(format!("[{}]", quoted.join(", ")))
}

/// Turns an `[mcp]` table's [`InjectionSpec::args`] into the argv elements a launch injects,
/// resolving each known placeholder: `server` for `{server}` (the MCP server
/// name, verbatim), `bridge_path` for `{bridge_toml}` (as a TOML literal
/// string), the fixed channel environment-variable names for
/// `{env_names_toml}` (as a TOML literal-string array,
/// [`yagura_protocol::MCP_CHANNEL_ENV_VARS`] followed by
/// [`yagura_protocol::MCP_BRIDGE_ENV`]) and for `{wslenv_toml}` (as one TOML
/// literal string, [`yagura_protocol::MCP_CHANNEL_ENV_VARS`] joined by `:` --
/// a `WSLENV` value), and `paths` for `{mcp_config_file}` /
/// `{mcp_config_dir}` (substituted verbatim -- they are path arguments, not
/// TOML content). An element with none of them passes through unchanged.
///
/// Returns `None` when `bridge_path` cannot be rendered as a TOML literal
/// string (it contains a single quote or a newline): emitting a broken TOML
/// value would be worse than refusing the injection outright.
#[must_use]
pub fn render_mcp_args(
    spec: &InjectionSpec,
    server: &str,
    bridge_path: &Path,
    paths: InjectedPaths<'_>,
) -> Option<Vec<String>> {
    let bridge_toml = toml_literal_string(&bridge_path.to_string_lossy())?;

    let mut env_names: Vec<String> = yagura_protocol::MCP_CHANNEL_ENV_VARS
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    env_names.push(yagura_protocol::MCP_BRIDGE_ENV.to_string());
    let env_name_refs: Vec<&str> = env_names.iter().map(String::as_str).collect();
    let env_names_toml = toml_literal_string_array(&env_name_refs)?;
    // The bridge path stays out: only the WSL-side shell reads it.
    let wslenv_toml = toml_literal_string(&yagura_protocol::MCP_CHANNEL_ENV_VARS.join(":"))?;

    let mut rendered = Vec::with_capacity(spec.args.len());
    for arg in &spec.args {
        // First, so a substituted path that happens to spell `{server}` is
        // never rewritten.
        let mut value = substitute_server(arg, server);
        value = value.replace(MCP_BRIDGE_TOML_PLACEHOLDER, &bridge_toml);
        value = value.replace(MCP_ENV_NAMES_TOML_PLACEHOLDER, &env_names_toml);
        value = value.replace(MCP_WSLENV_TOML_PLACEHOLDER, &wslenv_toml);
        rendered.push(substitute_paths(value, TableKind::Mcp, paths));
    }
    Some(rendered)
}

/// Replaces `kind`'s config-file and config-directory placeholders in `arg`
/// with the paths `paths` carries; a placeholder without a path stays.
fn substitute_paths(arg: String, kind: TableKind, paths: InjectedPaths<'_>) -> String {
    let mut value = arg;
    if let Some(path) = paths.config_file {
        value = value.replace(config_file_placeholder(kind), &path.to_string_lossy());
    }
    if let Some(path) = paths.config_dir {
        value = value.replace(config_dir_placeholder(kind), &path.to_string_lossy());
    }
    value
}

/// Replaces `{server}` in `text` with `server` and nothing else, so JSON
/// braces and `${NAME}` references in a settings file survive.
fn substitute_server(text: &str, server: &str) -> String {
    text.replace(MCP_SERVER_PLACEHOLDER, server)
}

/// Turns a `[hooks]` table's [`InjectionSpec::args`] into the argv elements a
/// launch injects: `paths` replace `{hooks_config_file}` and
/// `{hooks_config_dir}` (verbatim, as path arguments); an element without
/// them passes through unchanged.
#[must_use]
pub fn render_hooks_args(spec: &InjectionSpec, paths: InjectedPaths<'_>) -> Vec<String> {
    spec.args
        .iter()
        .map(|arg| substitute_paths(arg.clone(), TableKind::Hooks, paths))
        .collect()
}

/// Renders an `[mcp]` table into the injection one launch carries, naming
/// the MCP server `server` (the same name the instance registers under,
/// [`crate::mcp_registration::RegistrationTarget::server`]): its settings
/// file and directory, when it has them, have `{server}` substituted and are
/// named after the substituted content under `config_dir`, and the rendered
/// `args` name them ([`render_mcp_args`]) as the daemon sees them.
///
/// `server` goes into JSON keys and bare TOML keys unquoted, which holds
/// because an instance id is limited to `[a-z0-9-]`
/// (`yagura_os::validate_instance_id`).
///
/// Returns `None` -- no injection for this launch, with a warning -- when
/// the table needs a settings file but no `config_dir` was configured, or
/// `bridge_path` cannot be rendered.
#[must_use]
pub fn prepare_mcp_injection(
    spec: &InjectionSpec,
    server: &str,
    bridge_path: &Path,
    config_dir: Option<&Path>,
) -> Option<CliInjection> {
    plan_mcp_injection(spec, server, bridge_path, config_dir)?.render(Path::to_path_buf)
}

/// Renders a `[hooks]` table into the injection one launch carries, the same
/// way [`prepare_mcp_injection`] does for `[mcp]` but without a bridge: the
/// hooks reach the daemon through the window's own environment, so nothing
/// else has to be resolved first.
///
/// Returns `None` -- no injection for this launch, with a warning -- when
/// the table needs a settings file but no `config_dir` was configured.
#[must_use]
pub fn prepare_hooks_injection(
    spec: &InjectionSpec,
    config_dir: Option<&Path>,
) -> Option<CliInjection> {
    plan_hooks_injection(spec, config_dir)?.render(Path::to_path_buf)
}

/// [`prepare_mcp_injection`] with its `args` left unrendered, so the
/// daemon-side paths they name can be converted first
/// ([`UnrenderedInjection::host_paths`]).
#[must_use]
pub fn plan_mcp_injection(
    spec: &InjectionSpec,
    server: &str,
    bridge_path: &Path,
    config_dir: Option<&Path>,
) -> Option<UnrenderedInjection> {
    let settings = InjectionSettings {
        file: spec
            .config_file
            .as_deref()
            .map(|content| substitute_server(content, server)),
        dir: spec.config_dir.as_ref().map(|files| {
            files
                .iter()
                .map(|(name, content)| (name.clone(), substitute_server(content, server)))
                .collect()
        }),
    };
    let mcp = McpRendering {
        server: server.to_string(),
        bridge: bridge_path.to_path_buf(),
    };
    plan_injection(TableKind::Mcp, spec, Some(mcp), settings, config_dir)
}

/// [`prepare_hooks_injection`] with its `args` left unrendered, as
/// [`plan_mcp_injection`] leaves an `[mcp]` table's.
#[must_use]
pub fn plan_hooks_injection(
    spec: &InjectionSpec,
    config_dir: Option<&Path>,
) -> Option<UnrenderedInjection> {
    let settings = InjectionSettings {
        file: spec.config_file.clone(),
        dir: spec.config_dir.clone(),
    };
    plan_injection(TableKind::Hooks, spec, None, settings, config_dir)
}

/// The settings one launch writes for a table, after any substitution.
struct InjectionSettings {
    file: Option<String>,
    dir: Option<BTreeMap<String, String>>,
}

/// What an `[mcp]` table's `args` are rendered with besides paths.
#[derive(Debug, Clone)]
struct McpRendering {
    server: String,
    bridge: PathBuf,
}

/// An `[mcp]` or `[hooks]` table prepared for one launch: its settings file
/// and directory are named, but its `args` are not rendered yet, because the
/// paths they name may first have to be turned into the paths the CLI sees
/// (the profile's `[host_paths]`).
#[derive(Debug, Clone)]
pub struct UnrenderedInjection {
    kind: TableKind,
    spec: InjectionSpec,
    mcp: Option<McpRendering>,
    config_file: Option<InjectedConfigFile>,
    config_dir: Option<InjectedConfigDir>,
}

impl UnrenderedInjection {
    /// The table this injection comes from.
    #[must_use]
    pub fn kind(&self) -> InjectionKind {
        self.kind.injection_kind()
    }

    /// The daemon-side paths the rendered `args` would name: the settings
    /// file, the settings directory and the bridge, each only when an
    /// element spells its placeholder. Empty when the `args` name no path.
    #[must_use]
    pub fn host_paths(&self) -> Vec<PathBuf> {
        let named = |placeholder: &str| self.spec.args.iter().any(|arg| arg.contains(placeholder));
        let mut paths = Vec::new();
        if let Some(file) = &self.config_file
            && named(config_file_placeholder(self.kind))
        {
            paths.push(file.path());
        }
        if let Some(dir) = &self.config_dir
            && named(config_dir_placeholder(self.kind))
        {
            paths.push(dir.path());
        }
        if let Some(mcp) = &self.mcp
            && named(MCP_BRIDGE_TOML_PLACEHOLDER)
        {
            paths.push(mcp.bridge.clone());
        }
        paths
    }

    /// Renders the `args`, naming each daemon-side path as `seen_as` turns
    /// it. `None` -- with a warning -- when an `[mcp]` table's bridge path
    /// cannot be rendered.
    #[must_use]
    pub fn render(self, seen_as: impl Fn(&Path) -> PathBuf) -> Option<CliInjection> {
        let file_path = self.config_file.as_ref().map(|file| seen_as(&file.path()));
        let dir_path = self.config_dir.as_ref().map(|dir| seen_as(&dir.path()));
        let paths = InjectedPaths {
            config_file: file_path.as_deref(),
            config_dir: dir_path.as_deref(),
        };
        let args = match &self.mcp {
            Some(mcp) => {
                let bridge = seen_as(&mcp.bridge);
                let args = render_mcp_args(&self.spec, &mcp.server, &bridge, paths);
                if args.is_none() {
                    tracing::warn!(
                        "the MCP bridge path cannot be rendered as a TOML literal string; the \
                         window opens without MCP injection"
                    );
                }
                args?
            }
            None => render_hooks_args(&self.spec, paths),
        };
        Some(CliInjection {
            kind: self.kind.injection_kind(),
            args,
            config_file: self.config_file,
            config_dir: self.config_dir,
        })
    }
}

/// The part of planning an injection both kinds share: the settings file
/// and directory in `settings`, each named after its content under
/// `config_dir`.
fn plan_injection(
    kind: TableKind,
    spec: &InjectionSpec,
    mcp: Option<McpRendering>,
    settings: InjectionSettings,
    config_dir: Option<&Path>,
) -> Option<UnrenderedInjection> {
    let InjectionSettings { file, dir } = settings;
    if (file.is_some() || dir.is_some()) && config_dir.is_none() {
        tracing::warn!(
            "no settings directory is configured; the window opens without {} injection",
            kind.injection_kind().label()
        );
        return None;
    }
    let config_file = file
        .zip(config_dir)
        .map(|(content, parent)| InjectedConfigFile {
            dir: parent.to_path_buf(),
            file_name: config_file_name(kind, &content),
            content,
        });
    let config_dir = dir
        .zip(config_dir)
        .map(|(files, parent)| InjectedConfigDir {
            parent: parent.to_path_buf(),
            dir_name: config_dir_name(kind, &files),
            files,
        });
    Some(UnrenderedInjection {
        kind,
        spec: spec.clone(),
        mcp,
        config_file,
        config_dir,
    })
}

fn kind_prefix(kind: TableKind) -> &'static str {
    match kind {
        TableKind::Mcp => "mcp",
        TableKind::Hooks => "hooks",
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `<mcp|hooks>-<sha-256 of content, hex>.json`: equal content always maps to
/// the same name, so a file already present under it never needs rewriting.
fn config_file_name(kind: TableKind, content: &str) -> String {
    format!(
        "{}-{}.json",
        kind_prefix(kind),
        sha256_hex(content.as_bytes())
    )
}

/// `<mcp|hooks>-<sha-256, hex>`, hashed over every (name, content) pair in
/// name order, each part prefixed with its length so no two different sets
/// of files hash the same bytes. No extension, so it never collides with a
/// [`config_file_name`].
fn config_dir_name(kind: TableKind, files: &BTreeMap<String, String>) -> String {
    let mut bytes = Vec::new();
    for (name, content) in files {
        for part in [name.as_bytes(), content.as_bytes()] {
            bytes.extend_from_slice(&(part.len() as u64).to_le_bytes());
            bytes.extend_from_slice(part);
        }
    }
    format!("{}-{}", kind_prefix(kind), sha256_hex(&bytes))
}

/// The wire form of one mapped axis: `None` for an axis the profile does not
/// map, otherwise its alias table in file order (the `args` template stays
/// the author's concern).
fn launch_option_info(spec: Option<&LaunchOptionSpec>) -> Option<LaunchOptionInfo> {
    spec.map(|spec| LaunchOptionInfo {
        aliases: spec
            .aliases
            .iter()
            .map(|(alias, value)| LaunchOptionAlias {
                alias: alias.clone(),
                value: value.clone(),
            })
            .collect(),
    })
}

/// Selects the argv `command` names for `os`: the operating-system-specific
/// key first, `default` next, and `None` when neither is present (meaning
/// this profile cannot launch on `os` at all).
///
/// A thin wrapper around [`program::select_for_os`], the shared rule set the
/// GUI's own default-profile placement logic also uses.
#[must_use]
pub fn argv_for_os(command: &ProfileCommand, os: ProfileOs) -> Option<&[String]> {
    program::select_for_os(
        os,
        command.windows.as_deref(),
        command.macos.as_deref(),
        command.linux.as_deref(),
        command.default.as_deref(),
    )
}

/// The single place that turns a profile into an unresolved argv, so a
/// listing's [`ProfileEntry::command`] and [`resolve_launch`] can never
/// disagree.
fn launch_argv(
    profile: &Profile,
    config: &DaemonConfig,
) -> Result<Vec<String>, ProfileLaunchError> {
    match &profile.command {
        // Kept for any `Profile`, though a window launch never lands here:
        // listing and lookup leave out a profile with no argv for the host.
        Some(command) => argv_for_os(command, config.host_os)
            .map(<[String]>::to_vec)
            .ok_or(ProfileLaunchError::NoArgvForOperatingSystem),
        None => config
            .default_shell
            .as_ref()
            .map(|shell| vec![shell.to_string_lossy().into_owned()])
            .ok_or(ProfileLaunchError::NoDefaultShell),
    }
}

/// Wraps [`DaemonConfig`]'s function-pointer field as `&dyn Fn`-compatible
/// closure, in one place shared by [`unavailable_reason`] and
/// [`crate::window::WindowRegistry`]'s spawn path.
pub(crate) fn is_executable_predicate(config: &DaemonConfig) -> impl Fn(&Path) -> bool {
    let predicate = config.program_is_executable;
    move |path: &Path| predicate(path)
}

/// Why a profile cannot be launched right now, as [`unavailability`] decides
/// it: the kind, its reason text and the profile's own guidance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailability {
    /// The kind of reason. Never [`ProfileUnavailableKind::Unknown`].
    pub kind: ProfileUnavailableKind,
    /// The English reason text: one fixed sentence per kind naming no path,
    /// followed by every part of [`Unavailability::guidance`] when there is
    /// one, so a reader of this text alone still learns the remedy.
    pub reason: String,
    /// The profile's own `shim_guidance`, for
    /// [`ProfileUnavailableKind::NeedsNativeProgram`] only, and only when the
    /// profile declares one.
    pub guidance: Option<ProfileGuidance>,
}

impl Unavailability {
    fn fixed(kind: ProfileUnavailableKind, sentence: &str) -> Self {
        Self {
            kind,
            reason: sentence.to_string(),
            guidance: None,
        }
    }
}

/// Reports why `profile` cannot be launched right now, or `None` when it
/// can. The single function both [`list_profiles`] and [`resolve_launch`]'s
/// callers consult, so a listing and a launch attempt never disagree, and the
/// one place a reason's kind and its text are decided together.
///
/// The reason text is one of five fixed sentences, each naming no path:
/// executable not found, a metacharacter the command interpreter would
/// reinterpret, no argv for this operating system, no default shell resolved,
/// and a wake by command through a Windows `.cmd` / `.bat` file (followed by
/// the profile's own guidance). No argv for this operating system is answered
/// for any `profile` passed in, but never reaches a listing: [`list_profiles`]
/// leaves such a profile out. Whether the profile is switched off
/// ([`Profile::enabled`]) is not a reason here.
#[must_use]
pub fn unavailability(profile: &Profile, config: &DaemonConfig) -> Option<Unavailability> {
    let argv = match &profile.command {
        Some(command) => match argv_for_os(command, config.host_os) {
            Some(argv) => argv,
            None => {
                return Some(Unavailability::fixed(
                    ProfileUnavailableKind::NoCommand,
                    REASON_NO_ARGV,
                ));
            }
        },
        None => {
            return if config.default_shell.is_some() {
                None
            } else {
                Some(Unavailability::fixed(
                    ProfileUnavailableKind::NoDefaultShell,
                    REASON_NO_DEFAULT_SHELL,
                ))
            };
        }
    };

    let platform = config.host_os.platform();
    let predicate = is_executable_predicate(config);

    if program::is_available(argv, platform, &config.program_lookup, &predicate) {
        return wake_command_unavailability(profile, argv, config);
    }

    // `is_available` judged the argv unlaunchable; `resolve_program` is
    // asked again only to tell "rejected" (a metacharacter) apart from
    // "simply not found" for the reason text. This holds for a path-form
    // `.cmd` / `.bat` argv[0] too, which goes through the same check.
    Some(
        match program::resolve_program(argv, platform, &config.program_lookup, &predicate) {
            Err(_rejected) => {
                Unavailability::fixed(ProfileUnavailableKind::Metacharacter, REASON_METACHARACTER)
            }
            Ok(_) => Unavailability::fixed(ProfileUnavailableKind::NotFound, REASON_NOT_FOUND),
        },
    )
}

/// The reason text [`unavailability`] gives, or `None` when `profile` can be
/// launched right now.
#[must_use]
pub fn unavailable_reason(profile: &Profile, config: &DaemonConfig) -> Option<String> {
    unavailability(profile, config).map(|found| found.reason)
}

/// Why `profile`, whose `argv` can be started, still cannot be used: it wakes a
/// waiting CLI by running its own program with the text as an argument, and
/// that program is a Windows `.cmd` / `.bat` file, through which the text
/// cannot be passed unchanged. The profile's own guidance follows the fixed
/// sentence.
fn wake_command_unavailability(
    profile: &Profile,
    argv: &[String],
    config: &DaemonConfig,
) -> Option<Unavailability> {
    let delivery = profile.delivery.as_ref()?;
    if !matches!(delivery.wake, WakeMode::Command(_)) {
        return None;
    }
    let predicate = is_executable_predicate(config);
    let resolution = program::resolve_program(
        argv,
        config.host_os.platform(),
        &config.program_lookup,
        &predicate,
    )
    .ok()?;
    if !matches!(resolution, program::Resolution::CmdShim { .. }) {
        return None;
    }
    let guidance = delivery.shim_guidance.clone();
    Some(Unavailability {
        kind: ProfileUnavailableKind::NeedsNativeProgram,
        reason: match &guidance {
            Some(guidance) => format!(
                "{REASON_WAKE_NEEDS_NATIVE_PROGRAM}. {}",
                guidance_sentence(guidance)
            ),
            None => REASON_WAKE_NEEDS_NATIVE_PROGRAM.to_string(),
        },
        guidance,
    })
}

/// `guidance` as one line: the text, then the command, then the page.
fn guidance_sentence(guidance: &ProfileGuidance) -> String {
    let mut sentence = guidance.text.clone();
    if let Some(command) = &guidance.command {
        sentence.push(' ');
        sentence.push_str(command);
    }
    if let Some(url) = &guidance.url {
        sentence.push_str(" (see ");
        sentence.push_str(url);
        sentence.push(')');
    }
    sentence
}

/// English reason text for what the listing side can confirm about this
/// profile's detection, or `None` when it has nothing to report.
///
/// Ordered, because the conditions are not exclusive, and each order states
/// only what it has actually established: a name breaking the naming rule is
/// checked first, because opening a window from it is refused whatever the
/// layers hold; then the absence of any layer, which pins every window to
/// `Unknown` whether or not the profile names a definition; then the
/// inventory, which [`DetectionLayers::list`] collects without reading any
/// definition file, so an inventory it could not scan is reported as such
/// rather than as a confirmed miss.
fn detection_definition_warning(detection: Option<&str>, config: &DaemonConfig) -> Option<String> {
    let layers = DetectionLayers::from_config(config);

    if let Some(name) = detection
        && !is_valid_detection_name(name)
    {
        return Some(format!(
            "detection definition name {name:?} is invalid; no window can be opened from this \
             profile"
        ));
    }

    if layers.is_disabled() {
        // Reusing the resolver's own sentence keeps the listing side and the
        // window side from drifting into two wordings for one fact.
        return Some(LayerError::Disabled.to_string());
    }

    let name = detection?;
    let listing = layers.list();
    if listing.names.iter().any(|candidate| candidate == name) {
        return None;
    }
    if listing.warning.is_some() {
        return Some(
            LayerError::Unscannable {
                name: name.to_string(),
            }
            .to_string(),
        );
    }
    Some(format!(
        "detection definition {name:?} was not found; a window opened from this profile would \
         stay unknown"
    ))
}

/// Logs a profile failure with its [`std::error::Error::source`] chain: that
/// chain and `path` carry the detail a client's reply must never hold.
fn warn_profile_io(context: &str, path: &Path, err: &dyn std::error::Error) {
    tracing::warn!(
        "failed to {context} {}: {}",
        path.display(),
        error_chain(err)
    );
}

/// Whether `profile` has something to launch on the host operating system: no
/// `[command]` table (the default shell), or one naming an argv for the host.
/// A profile failing this is left out of the listing and unknown to every
/// lookup by id, without a warning: it is simply not for this host.
fn has_command_for_host(profile: &Profile, config: &DaemonConfig) -> bool {
    match &profile.command {
        Some(command) => argv_for_os(command, config.host_os).is_some(),
        None => true,
    }
}

/// Reads `dir`, returning the `(file_name, id)` pairs of every `.toml`
/// entry, without parsing them.
fn toml_entries(dir: &Path) -> std::io::Result<Vec<(std::ffi::OsString, String)>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(std::ffi::OsStr::to_str) else {
            continue;
        };
        entries.push((entry.file_name(), stem.to_string()));
    }
    Ok(entries)
}

/// The user's files in the profiles directory. A directory the shipped
/// profiles are laid under need not exist yet; any other failure to read it
/// is the error.
fn user_entries(config: &DaemonConfig) -> std::io::Result<Vec<(std::ffi::OsString, String)>> {
    let Some(dir) = config.profiles_dir.as_deref() else {
        return Ok(Vec::new());
    };
    match toml_entries(dir) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && config.bundled_profiles => {
            Ok(Vec::new())
        }
        other => other,
    }
}

/// Whether `config` names anywhere profiles come from: a profiles directory,
/// or the shipped profiles.
pub(crate) fn has_profile_source(config: &DaemonConfig) -> bool {
    config.profiles_dir.is_some() || config.bundled_profiles
}

/// The shipped profile `profile_id` is laid over, when the shipped profiles
/// are a layer for `config`.
fn shipped_base(config: &DaemonConfig, profile_id: &str) -> Option<&'static str> {
    if config.bundled_profiles {
        shipped_profile_source(profile_id)
    } else {
        None
    }
}

/// Why a profile could not be read. The text never quotes the file.
#[derive(Debug, Error)]
enum ProfileReadError {
    #[error("could not be read")]
    Io(#[source] std::io::Error),
    #[error("is not valid TOML")]
    Toml(#[source] toml_edit::TomlError),
    #[error("{0}")]
    Parse(#[source] ProfileParseError),
}

/// `user` laid over `shipped`, parsed: the profile that is actually used.
fn parse_layered(
    profile_id: &str,
    shipped: Option<&str>,
    user: Option<&str>,
) -> Result<Profile, ProfileReadError> {
    let merged;
    let source = match (shipped, user) {
        (Some(shipped), Some(user)) => {
            merged = crate::profile_layers::overlay_source(shipped, user)
                .map_err(ProfileReadError::Toml)?;
            merged.as_str()
        }
        (Some(only), None) | (None, Some(only)) => only,
        (None, None) => "",
    };
    parse_profile(profile_id, source).map_err(ProfileReadError::Parse)
}

/// Reads the profile `profile_id` names: its user file (`file_name` in the
/// profiles directory, when the directory has one) laid over its shipped
/// base. `Ok(None)` when it has neither.
fn read_profile(
    config: &DaemonConfig,
    profile_id: &str,
    file_name: Option<&std::ffi::OsStr>,
) -> Result<Option<ListedProfileFile>, ProfileReadError> {
    let shipped = shipped_base(config, profile_id);
    let dir = config.profiles_dir.as_deref();
    let path = dir.map(|dir| match file_name {
        Some(file_name) => dir.join(file_name),
        None => dir.join(format!("{profile_id}.toml")),
    });
    let source = match (file_name, &path) {
        (Some(_), Some(path)) => Some(std::fs::read_to_string(path).map_err(ProfileReadError::Io)?),
        _ => None,
    };
    if shipped.is_none() && source.is_none() {
        return Ok(None);
    }
    let profile = parse_layered(profile_id, shipped, source.as_deref())?;
    Ok(Some(ListedProfileFile {
        path,
        source: source.unwrap_or_default(),
        shipped,
        profile,
    }))
}

/// Enumerates the profiles, reading the profiles directory fresh on every
/// call, so an operator's edit takes effect without a daemon restart: every
/// shipped profile (when they are a layer) and every file in the directory,
/// a file laid over the shipped profile of the same id.
///
/// No configured source at all, and a configured directory that cannot be
/// read, leave the listing empty with a warning: without the user's layer a
/// shipped profile would be offered as the user never left it, switched-off
/// ones included, and [`read_listed_profile`] refuses it all the same. A
/// profile whose result does not parse is left out with a warning; its
/// shipped base is not used in its place.
#[must_use]
pub fn list_profiles(config: &DaemonConfig) -> ProfileListing {
    if !has_profile_source(config) {
        return ProfileListing {
            profiles: Vec::new(),
            warnings: vec!["no profiles directory is configured".to_string()],
        };
    }
    let mut files: BTreeMap<String, std::ffi::OsString> = match user_entries(config) {
        Ok(entries) => entries
            .into_iter()
            .map(|(file_name, id)| (id, file_name))
            .collect(),
        Err(err) => {
            if let Some(dir) = config.profiles_dir.as_deref() {
                warn_profile_io("read the profiles directory", dir, &err);
            }
            return ProfileListing {
                profiles: Vec::new(),
                warnings: vec!["the configured profiles directory could not be read".to_string()],
            };
        }
    };
    let mut warnings = Vec::new();
    let mut ids: std::collections::BTreeSet<String> = files.keys().cloned().collect();
    if config.bundled_profiles {
        ids.extend(BUNDLED_PROFILES.iter().map(|(id, _)| (*id).to_string()));
    }

    let mut profiles = Vec::new();
    for id in ids {
        let file_name = files.remove(&id);
        match read_profile(config, &id, file_name.as_deref()) {
            Ok(Some(listed)) if has_command_for_host(&listed.profile, config) => {
                profiles.push(listed_entry(listed.profile, config));
            }
            Ok(_) => {}
            Err(err) => {
                // A shipped profile is read from `profiles/<id>.toml` too, so
                // both layers are named by the same file name.
                let file_name = file_name.unwrap_or_else(|| format!("{id}.toml").into());
                tracing::warn!(
                    "failed to read profile {file_name:?}: {}",
                    error_chain(&err)
                );
                warnings.push(format!("profile file {file_name:?} {err}"));
            }
        }
    }

    ProfileListing { profiles, warnings }
}

/// `err` followed by its [`std::error::Error::source`] chain.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut detail = err.to_string();
    let mut cause = err.source();
    while let Some(source) = cause {
        detail.push_str(": ");
        detail.push_str(&source.to_string());
        cause = source.source();
    }
    detail
}

/// The listing entry for `profile`.
fn listed_entry(profile: Profile, config: &DaemonConfig) -> ProfileEntry {
    let (unavailable_kind, unavailable_reason, unavailable_guidance) =
        match unavailability(&profile, config) {
            Some(found) => (Some(found.kind), Some(found.reason), found.guidance),
            None => (None, None, None),
        };
    let command = listed_argv(&profile, config);
    ProfileEntry {
        id: profile.id,
        display_name: profile.display_name,
        detection_warning: detection_definition_warning(profile.detection.as_deref(), config),
        detection: profile.detection,
        cwd: profile.cwd,
        command,
        enabled: profile.enabled,
        available: unavailable_reason.is_none(),
        unavailable_reason,
        unavailable_kind,
        unavailable_guidance,
        model: launch_option_info(profile.options.model.as_ref()),
        effort: launch_option_info(profile.options.effort.as_ref()),
    }
}

/// The argv a listing shows for `profile`. A default-shell profile with no
/// shell resolved is still listed (with its reason), so the absent argv
/// becomes an empty one rather than dropping the entry.
pub(crate) fn listed_argv(profile: &Profile, config: &DaemonConfig) -> Vec<String> {
    launch_argv(profile, config).unwrap_or_default()
}

/// Finds and parses the one profile `profile_id` names (see
/// [`read_listed_profile`]).
fn find_listed_profile(
    config: &DaemonConfig,
    profile_id: &str,
) -> Result<Profile, ProfileLaunchError> {
    read_listed_profile(config, profile_id)
        .map(|file| file.profile)
        .ok_or(ProfileLaunchError::UnknownProfile)
}

/// Refuses a listed profile its `enabled` key switches off. Applied after
/// [`find_listed_profile`] by everything that starts a profile, and never
/// inside [`read_listed_profile`], which the rewrite path shares: a reader
/// that dropped such a profile could never switch it back on.
fn refuse_if_disabled(profile: Profile) -> Result<Profile, ProfileLaunchError> {
    if profile.enabled {
        Ok(profile)
    } else {
        Err(ProfileLaunchError::Disabled)
    }
}

/// A listed profile together with the layers it was read from.
pub(crate) struct ListedProfileFile {
    /// Where the user's file for this profile lives in the profiles
    /// directory, whether or not it exists yet; `None` with no directory.
    pub(crate) path: Option<PathBuf>,
    /// The user's file exactly as read, empty when there is none.
    pub(crate) source: String,
    /// The shipped profile `source` is laid over, when there is one.
    pub(crate) shipped: Option<&'static str>,
    /// `source` laid over `shipped`, parsed.
    pub(crate) profile: Profile,
}

impl ListedProfileFile {
    /// The profile as it would read with `user_source` as its user file, laid
    /// over the same shipped base.
    pub(crate) fn reread_with(&self, user_source: &str) -> Option<Profile> {
        parse_layered(&self.profile.id, self.shipped, Some(user_source)).ok()
    }
}

/// Reads and parses the one profile `profile_id` names, its user file matched
/// against the directory's own entries by file-stem equality rather than
/// joined onto the directory, so it can never reach a path outside it. `None`
/// (with the cause logged) wherever [`list_profiles`] would not report the
/// profile: an invalid id, neither a shipped profile nor a file by that id, a
/// result that cannot be read or parsed, or a profile with nothing to launch
/// on the host operating system (not logged: nothing is wrong with it).
pub(crate) fn read_listed_profile(
    config: &DaemonConfig,
    profile_id: &str,
) -> Option<ListedProfileFile> {
    if !validate_profile_id(profile_id) {
        return None;
    }
    let entries = user_entries(config)
        .map_err(|err| {
            if let Some(dir) = config.profiles_dir.as_deref() {
                warn_profile_io("read the profiles directory", dir, &err);
            }
        })
        .ok()?;
    let file_name = entries
        .into_iter()
        .find(|(_, id)| id == profile_id)
        .map(|(file_name, _)| file_name);
    read_profile(config, profile_id, file_name.as_deref())
        .map_err(|err| {
            tracing::warn!(
                "failed to read profile {profile_id:?}: {}",
                error_chain(&err)
            );
        })
        .ok()
        .flatten()
        .filter(|listed| has_command_for_host(&listed.profile, config))
}

// Shipped profiles generated by `build.rs`.
include!(concat!(env!("OUT_DIR"), "/bundled_profiles.rs"));

/// The text of the profile shipped with this build under `profile_id`, or
/// `None` when no shipped profile has that id.
pub(crate) fn shipped_profile_source(profile_id: &str) -> Option<&'static str> {
    BUNDLED_PROFILES
        .iter()
        .find(|(id, _)| *id == profile_id)
        .map(|(_, source)| *source)
}

/// Resolves `profile_id` into the content of a window launch, reading the
/// profile fresh on every call, so an edit since the last launch is picked up.
///
/// Availability ([`unavailable_reason`]) is *not* checked here: a profile
/// naming an executable the injected lookup cannot currently find still
/// resolves, exactly like [`crate::window::WindowRegistry::open`] does not
/// pre-check an explicit command's existence -- the eventual spawn attempt
/// answers `spawn_failed` on its own. What *is* checked is structural:
/// whether there is any argv to launch at all.
///
/// # Errors
///
/// Returns [`ProfileLaunchError::UnknownProfile`] for an identifier
/// [`list_profiles`] would not report (including a path-traversal attempt,
/// rejected by its character set before the directory is read, and a profile
/// whose `[command]` table names no argv for [`DaemonConfig::host_os`]),
/// [`ProfileLaunchError::Disabled`] when the profile's `enabled` key switches
/// it off, [`ProfileLaunchError::NoDefaultShell`] when the profile omits
/// `[command]` but no default shell was resolved at start-up, and
/// [`ProfileLaunchError::UnsupportedLaunchOption`] when `options` names an
/// axis the profile has no table for (see [`apply_launch_options`]).
pub fn resolve_launch(
    config: &DaemonConfig,
    profile_id: &str,
    cwd: Option<PathBuf>,
    name: Option<String>,
    options: &LaunchOptionValues,
) -> Result<ProfileLaunch, ProfileLaunchError> {
    let profile = refuse_if_disabled(find_listed_profile(config, profile_id)?)?;
    let mut argv = launch_argv(&profile, config)?;
    apply_launch_options(&mut argv, &profile.options, options)?;
    let hooks_registration = profile
        .mcp_registration
        .clone()
        .filter(|registration| profile.hooks.is_some() || registration.json_entry().is_some());
    let bridge_listing = profile
        .mcp_registration
        .as_ref()
        .and_then(|registration| registration.list().cloned());

    Ok(ProfileLaunch {
        argv,
        // Written as a match rather than `cwd.or(profile.cwd)`: which of the
        // two a launch ends up with decides whether a refusal may name the
        // caller's own argument, and that must not rest on argument order.
        cwd: match cwd {
            Some(requested) => Some(RequestedCwd::from_caller(requested)),
            None => profile.cwd.map(RequestedCwd::from_profile),
        },
        name: name.unwrap_or(profile.display_name),
        vendor: profile.id,
        detection: profile.detection,
        env: profile.env,
        mcp: profile.mcp,
        hooks: profile.hooks,
        hooks_registration,
        bridge_listing,
        host_paths: profile.host_paths,
        delivery: profile.delivery,
    })
}

/// Resolves `argv` into the argv a one-shot child is actually spawned with,
/// through the same program resolution a window's launch uses
/// ([`program::resolve_program`], then [`program::into_spawn_argv`]): a
/// `.cmd` / `.bat` shim is started through the command interpreter, and an
/// argv the interpreter would reinterpret is refused before anything starts.
/// An executable the lookup cannot find is not refused here; the spawn
/// itself fails on it.
///
/// # Errors
///
/// Returns the fixed metacharacter reason sentence (the same one
/// [`unavailable_reason`] gives) when the command interpreter would
/// reinterpret part of `argv`.
pub(crate) fn resolve_spawn_argv(
    config: &DaemonConfig,
    argv: &[String],
) -> Result<Vec<String>, &'static str> {
    let platform = config.host_os.platform();
    let predicate = is_executable_predicate(config);
    let resolution = program::resolve_program(argv, platform, &config.program_lookup, &predicate)
        .map_err(|_rejected| REASON_METACHARACTER)?;
    Ok(program::into_spawn_argv(&resolution, argv, platform))
}

/// Resolves where `profile_id`'s model choices come from, reading the profile
/// fresh on every call. A listing command is its launch argv followed by the
/// choices `args`, resolved for spawning exactly as a window's argv is, so a
/// window and a listing never disagree on which program starts.
///
/// # Errors
///
/// Returns [`ModelChoicesError::UnknownProfile`] for an identifier
/// [`list_profiles`] would not report, [`ModelChoicesError::Unavailable`]
/// carrying [`ProfileLaunchError::Disabled`]'s sentence when the profile's
/// `enabled` key switches it off, [`ModelChoicesError::NoModelOption`]
/// when the profile has no `[model]` table, and
/// [`ModelChoicesError::Unavailable`] when a listing command cannot be
/// started: [`unavailable_reason`] reports a reason, or the command
/// interpreter would reinterpret the full argv.
pub fn resolve_model_choices(
    config: &DaemonConfig,
    profile_id: &str,
) -> Result<ModelChoicesPlan, ModelChoicesError> {
    let profile =
        find_listed_profile(config, profile_id).map_err(|_| ModelChoicesError::UnknownProfile)?;
    let mut profile = refuse_if_disabled(profile)
        .map_err(|err| ModelChoicesError::Unavailable(err.to_string()))?;
    if profile.options.model.is_none() {
        return Err(ModelChoicesError::NoModelOption);
    }
    let (args, format) = match profile.options.model_choices.take() {
        None => return Ok(ModelChoicesPlan::NoChoices),
        Some(ModelChoicesSpec::Values(values)) => return Ok(ModelChoicesPlan::Values(values)),
        Some(ModelChoicesSpec::Command { args, format }) => (args, format),
    };
    if let Some(reason) = unavailable_reason(&profile, config) {
        return Err(ModelChoicesError::Unavailable(reason));
    }
    let launch_argv = launch_argv(&profile, config)
        .map_err(|err| ModelChoicesError::Unavailable(err.to_string()))?;
    let mut argv = launch_argv.clone();
    argv.extend(args);

    let spawn_argv = resolve_spawn_argv(config, &argv)
        .map_err(|reason| ModelChoicesError::Unavailable(reason.to_string()))?;

    Ok(ModelChoicesPlan::Command {
        argv,
        spawn_argv,
        launch_argv,
        env: profile.env.into_iter().collect(),
        cwd: profile.cwd,
        format,
    })
}

/// Appends the argv elements every supplied option maps to, in axis order
/// ([`LaunchOptionAxis::ALL`]). Pure: shared by [`resolve_launch`] and its
/// tests.
///
/// Every supplied axis is checked against `specs` before any element is
/// appended, so on `Err` `argv` is exactly what the caller passed in: a
/// refusal never leaves a half-built argv behind.
///
/// # Errors
///
/// Returns [`ProfileLaunchError::UnsupportedLaunchOption`] naming the first
/// axis (in [`LaunchOptionAxis::ALL`] order) that `values` supplies but
/// `specs` does not map.
pub fn apply_launch_options(
    argv: &mut Vec<String>,
    specs: &LaunchOptions,
    values: &LaunchOptionValues,
) -> Result<(), ProfileLaunchError> {
    let mut appended = Vec::new();
    for axis in LaunchOptionAxis::ALL {
        let Some(value) = values.get(axis) else {
            continue;
        };
        let spec = specs
            .get(axis)
            .ok_or(ProfileLaunchError::UnsupportedLaunchOption(axis))?;
        let resolved = resolve_option_value(spec, value);
        appended.extend(
            spec.args
                .iter()
                .map(|arg| arg.replace(VALUE_PLACEHOLDER, resolved)),
        );
    }
    argv.extend(appended);
    Ok(())
}

/// The value a caller's `value` maps to under `spec`: the alias's target when
/// `value` names one (matched exactly, one step only), otherwise `value`
/// itself (pass-through). Pure.
#[must_use]
pub fn resolve_option_value<'a>(spec: &'a LaunchOptionSpec, value: &'a str) -> &'a str {
    spec.aliases
        .iter()
        .find(|(alias, _)| alias == value)
        .map_or(value, |(_, target)| target.as_str())
}
