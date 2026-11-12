//! Detection-definition schema and TOML loader.
//!
//! A pure parse layer: it turns a definition TOML document into a validated,
//! compiled [`Definition`] and nothing else. It reads no
//! [`crate::DaemonConfig`] and applies no defaults for omitted optional
//! values (`bottom_line_count` stays `None` when absent) -- those belong to
//! the detection engine -- so the loader stays usable from tests and tooling
//! without pulling in the rest of the daemon.
//!
//! Rejection is always fail-loud: definition files are an externally
//! editable trust boundary, so a malformed, oversized or semantically
//! invalid one is returned as a typed [`DefinitionError`] rather than
//! silently coerced or partially applied.

use std::path::{Path, PathBuf};

use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use thiserror::Error;
use yagura_protocol::WindowStatus;

/// Schema version this loader accepts. Any other value is rejected so that a
/// future incompatible schema change fails loudly instead of being
/// misinterpreted.
const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Maximum accepted size (in bytes) of a definition source document, which
/// is externally editable and therefore untrusted.
const MAX_SOURCE_BYTES: usize = 256 * 1024;

/// Maximum accepted number of `[[rules]]` entries in a single definition.
const MAX_RULES: usize = 200;

/// `RegexBuilder::size_limit` applied to every compiled rule pattern.
const REGEX_SIZE_LIMIT_BYTES: usize = 1024 * 1024;

/// `RegexBuilder::dfa_size_limit` applied to every compiled rule pattern.
const REGEX_DFA_SIZE_LIMIT_BYTES: usize = 10 * 1024 * 1024;

/// Validates a `--detection <name>` value (and the equivalent
/// `DaemonConfig::detection_name` value received over config/IPC).
///
/// Delegates to [`yagura_protocol::is_valid_token`], the single
/// implementation of the `[a-z0-9_-]{1,64}` charset rule: the validated
/// value is used to build a filesystem path (`<detection_dir>/<name>.toml`),
/// so it must admit no path separator, `..`, uppercase or empty input.
/// Callers must not re-implement the check.
pub fn is_valid_detection_name(name: &str) -> bool {
    yagura_protocol::is_valid_token(name)
}

/// The region of the screen a [`Rule`] pattern is matched against.
///
/// `"bottom"` and `"screen"` are the values; any other TOML string (including
/// `"full"`) is a rejected, unknown value rather than silently ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    /// Match against the window ending at the last line holding a visible
    /// (non-whitespace) character, `bottom_line_count` lines tall.
    Bottom,
    /// Match against every row of the screen, one row at a time, with the
    /// composer's body taken out as for `Bottom`: for text whose distance
    /// from the bottom changes with the window's height.
    Screen,
}

/// Debounce configuration: how long a raw judgement must hold before it is
/// confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Debounce {
    /// Milliseconds a raw judgement must persist before becoming the
    /// confirmed status.
    pub confirm_ms: u64,
}

/// Opt-in time-based judgement for quiescence (no output for a while).
///
/// Present only when the definition's `[quiescence]` table is; both keys are
/// required once the table exists, so this struct has no optional fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quiescence {
    /// The status to report once quiescence is reached.
    pub status: WindowStatus,
    /// Milliseconds of no new output required before quiescence applies.
    pub after_ms: u64,
}

/// Opt-in time-based judgement for activity (output still flowing, no
/// pattern match).
///
/// See [`Quiescence`] for why both keys are required once the table exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activity {
    /// The status to report while activity is ongoing.
    pub status: WindowStatus,
    /// Milliseconds since the last output chunk during which activity
    /// still applies.
    pub window_ms: u64,
}

/// Raw values from a definition's optional `[composer]` table: how to identify
/// the composer (input area) on the screen.
///
/// `marker`, `separator` and `continuation_prefix` are required: if the
/// `[composer]` table is present, all three keys must be provided.
/// `blank_lines_in_body` is optional and defaults to `false`; `left_panel` is
/// optional and has no default.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComposerRawSection {
    /// A regex pattern string to identify the composer's marker line
    /// (e.g., the prompt). The pattern should match the prompt/marker itself
    /// but not include the separator in the pattern string (the separator is
    /// handled separately), unless `separator` is empty: then the marker
    /// carries the boundary itself and the body starts right after its match.
    pub marker: String,
    /// The string that separates the marker/prompt from the actual body text
    /// (e.g., a space or NBSP character). May be empty when the marker
    /// already ends at the body.
    pub separator: String,
    /// The string that prefixes continuation lines (lines wrapped to the next
    /// display line). May be an empty string for definitions where continuation
    /// lines have no special prefix.
    pub continuation_prefix: String,
    /// See [`ComposerSection::blank_lines_in_body`].
    #[serde(default)]
    pub blank_lines_in_body: bool,
    /// A regex pattern string; see [`ComposerSection::left_panel`].
    #[serde(default)]
    pub left_panel: Option<String>,
    /// Deprecated: told a box's edge from typed text for a daemon that read
    /// the composer before typing into it. Read whatever it holds, warned
    /// about and not used.
    #[serde(default)]
    pub right_border: Option<serde::de::IgnoredAny>,
    /// Deprecated: told an empty composer from a typed one for a daemon that
    /// typed into it. Read whatever it holds, warned about and not used.
    #[serde(default)]
    pub placeholder: Option<serde::de::IgnoredAny>,
}

/// The compiled section of a definition that identifies the composer (input area)
/// on the screen, so the rules can be matched without the text typed into it.
///
/// The marker pattern is already compiled to a [`Regex`], ready for use.
#[derive(Debug, Clone)]
pub struct ComposerSection {
    /// A compiled regex pattern to identify the composer's marker line
    /// (e.g., the prompt).
    pub marker: Regex,
    /// The string that separates the marker/prompt from the actual body text
    /// (e.g., a space or NBSP character). May be empty when the marker
    /// already ends at the body.
    pub separator: String,
    /// The string that prefixes continuation lines (lines wrapped to the next
    /// display line). May be an empty string for definitions where continuation
    /// lines have no special prefix.
    pub continuation_prefix: String,
    /// Whether a blank row above the cursor belongs to the body. A CLI that
    /// draws a blank body line as nothing, not as `continuation_prefix`, sets
    /// it; only a CLI whose dialogs hide the cursor can, because the walk up
    /// from the cursor then crosses rows that a dialog's layout has as well.
    pub blank_lines_in_body: bool,
    /// A compiled regex matching, from the start of a row, a panel the CLI
    /// draws to the left of the composer. Only when it matches every row
    /// holding a visible character and all its matches end at the same
    /// column is the composer read from that column; otherwise from column 0.
    pub left_panel: Option<Regex>,
}

/// The keys of a definition's `[input]` table, which told how the daemon was
/// to type into the CLI. Nothing is typed into a window any more: each key is
/// read whatever it holds, warned about and not used. A key outside these is
/// still a load error, so a mistyped key keeps failing loudly.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInputSection {
    bracketed_paste: Option<serde::de::IgnoredAny>,
    submit: Option<serde::de::IgnoredAny>,
    ready_hold_ms: Option<serde::de::IgnoredAny>,
    submit_delay_ms: Option<serde::de::IgnoredAny>,
    clear: Option<serde::de::IgnoredAny>,
    delivery_confirm_ms: Option<serde::de::IgnoredAny>,
    resend_max: Option<serde::de::IgnoredAny>,
    reflect_wait_ms: Option<serde::de::IgnoredAny>,
    probe: Option<serde::de::IgnoredAny>,
    erase: Option<serde::de::IgnoredAny>,
    leading_keys: Option<serde::de::IgnoredAny>,
}

impl RawInputSection {
    /// The keys the table writes.
    fn written(&self) -> Vec<&'static str> {
        [
            ("bracketed_paste", self.bracketed_paste.is_some()),
            ("submit", self.submit.is_some()),
            ("ready_hold_ms", self.ready_hold_ms.is_some()),
            ("submit_delay_ms", self.submit_delay_ms.is_some()),
            ("clear", self.clear.is_some()),
            ("delivery_confirm_ms", self.delivery_confirm_ms.is_some()),
            ("resend_max", self.resend_max.is_some()),
            ("reflect_wait_ms", self.reflect_wait_ms.is_some()),
            ("probe", self.probe.is_some()),
            ("erase", self.erase.is_some()),
            ("leading_keys", self.leading_keys.is_some()),
        ]
        .into_iter()
        .filter_map(|(key, written)| written.then_some(key))
        .collect()
    }
}

/// A single compiled pattern rule.
///
/// `pattern` is a compiled [`regex::Regex`] (not the raw TOML string), so a
/// loaded rule can be matched directly against display text without the
/// caller recompiling anything.
#[derive(Debug, Clone)]
pub struct Rule {
    /// The status this rule reports when `pattern` matches.
    pub status: WindowStatus,
    /// The compiled display-text pattern.
    pub pattern: Regex,
    /// Numerically larger wins when multiple rules match the same text
    /// (deterministic tie-breaking).
    pub priority: i64,
    /// The screen region `pattern` is matched against.
    pub region: Region,
}

/// A fully parsed and validated detection definition.
#[derive(Debug, Clone)]
pub struct Definition {
    /// The schema version declared by the source document (always
    /// [`SUPPORTED_SCHEMA_VERSION`] for a successfully parsed definition).
    pub schema_version: u32,
    /// The definition's name. Vendor identity lives here as data, never in a
    /// type or field name.
    pub name: String,
    /// The height of the window `Region::Bottom` matches against, counted
    /// upward from the last line holding a visible (non-whitespace)
    /// character. Stays `None` when omitted: applying a default is the
    /// engine's responsibility, not the loader's.
    pub bottom_line_count: Option<u16>,
    /// Debounce configuration, if the definition overrides it.
    pub debounce: Option<Debounce>,
    /// The compiled rule set, in declaration order.
    pub rules: Vec<Rule>,
    /// Opt-in quiescence judgement, if the definition's `[quiescence]`
    /// table is present.
    pub quiescence: Option<Quiescence>,
    /// Opt-in activity judgement, if the definition's `[activity]` table
    /// is present.
    pub activity: Option<Activity>,
    /// Composer identification section, if the definition's `[composer]`
    /// table is present. Used to read the input area on screen.
    pub composer: Option<ComposerSection>,
    /// How this CLI treats a tool call it made; every field at its default
    /// when the `[requests]` table is omitted. See [`RequestsSection`].
    pub requests: RequestsSection,
}

/// The optional `[requests]` table: how a CLI treats a tool call of its own
/// that has not returned yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestsSection {
    /// Whether this CLI, on ending a turn, drops a tool call that has not
    /// returned without cancelling it. When true, a request this window made
    /// is treated as withdrawn once the window, having been `Running`, comes
    /// to rest (`WaitingInput`, `Interrupted`, `Idle` or `Exited`).
    #[serde(default)]
    pub abandoned_at_turn_end: bool,
}

/// Errors returned when a detection definition source is rejected.
///
/// Every rejection path (malformed TOML, unknown enum values, missing
/// required keys, oversized input, invalid regex, unsupported schema
/// version, I/O failure) is represented here so that callers always get a
/// typed error instead of a silently coerced or partially applied
/// definition (fail-loud).
#[derive(Debug, Error)]
pub enum DefinitionError {
    /// Reading the definition file from disk failed.
    #[error("failed to read detection definition at {path}: {source}")]
    Io {
        /// The path that failed to read.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The source document exceeds [`MAX_SOURCE_BYTES`].
    #[error("detection definition source is {actual} bytes, exceeding the {max}-byte limit")]
    SourceTooLarge {
        /// The maximum allowed size, in bytes.
        max: usize,
        /// The actual size of the rejected source, in bytes.
        actual: usize,
    },
    /// TOML parsing or schema validation (missing/unknown keys, wrong
    /// types, unknown enum values such as an unrecognized `status` or
    /// `region`) failed.
    #[error("failed to parse detection definition: {0}")]
    Toml(#[from] toml::de::Error),
    /// `schema_version` did not equal [`SUPPORTED_SCHEMA_VERSION`].
    #[error("unsupported schema_version {actual} (expected {expected})")]
    UnsupportedSchemaVersion {
        /// The schema version this loader supports.
        expected: u32,
        /// The schema version declared by the rejected document.
        actual: u32,
    },
    /// The `[[rules]]` array exceeds [`MAX_RULES`].
    #[error("detection definition has {actual} rules, exceeding the {max}-rule limit")]
    TooManyRules {
        /// The maximum allowed number of rules.
        max: usize,
        /// The actual number of rules in the rejected document.
        actual: usize,
    },
    /// A rule's `pattern` failed to compile as a regular expression.
    #[error("invalid regex pattern {pattern:?}: {source}")]
    InvalidPattern {
        /// The rejected pattern source.
        pattern: String,
        /// The underlying regex compilation error.
        #[source]
        source: regex::Error,
    },
}

/// Raw, pre-validation representation of `[[rules]]`. `pattern` is kept as a
/// string here since compiling it is a fallible step handled separately by
/// [`compile_rule`], distinct from the structural TOML/schema validation
/// `serde`/`toml` perform for every other field.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    status: WindowStatus,
    pattern: String,
    priority: i64,
    region: Region,
}

/// Raw, pre-validation representation of the whole document. Deserializing
/// this is where most rejection rules are enforced structurally: a missing
/// `name` or `rules` key, an unknown `status`/`region` value, or a
/// `[quiescence]`/`[activity]` table missing one of its required keys all
/// fail here as a [`toml::de::Error`], because none of these fields carry a
/// `#[serde(default)]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDefinition {
    schema_version: u32,
    name: String,
    #[serde(default)]
    bottom_line_count: Option<u16>,
    #[serde(default)]
    debounce: Option<Debounce>,
    rules: Vec<RawRule>,
    #[serde(default)]
    quiescence: Option<Quiescence>,
    #[serde(default)]
    activity: Option<Activity>,
    #[serde(default)]
    composer: Option<ComposerRawSection>,
    #[serde(default)]
    input: Option<RawInputSection>,
    #[serde(default)]
    requests: RequestsSection,
}

fn compile_composer_section(raw: ComposerRawSection) -> Result<ComposerSection, DefinitionError> {
    let marker = compile_composer_pattern(raw.marker)?;
    let left_panel = raw.left_panel.map(compile_composer_pattern).transpose()?;

    Ok(ComposerSection {
        marker,
        separator: raw.separator,
        continuation_prefix: raw.continuation_prefix,
        blank_lines_in_body: raw.blank_lines_in_body,
        left_panel,
    })
}

fn compile_composer_pattern(pattern: String) -> Result<Regex, DefinitionError> {
    RegexBuilder::new(&pattern)
        .size_limit(REGEX_SIZE_LIMIT_BYTES)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT_BYTES)
        .build()
        .map_err(|source| DefinitionError::InvalidPattern { pattern, source })
}

fn compile_rule(raw: RawRule) -> Result<Rule, DefinitionError> {
    let pattern = RegexBuilder::new(&raw.pattern)
        .size_limit(REGEX_SIZE_LIMIT_BYTES)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT_BYTES)
        .build()
        .map_err(|source| DefinitionError::InvalidPattern {
            pattern: raw.pattern,
            source,
        })?;

    Ok(Rule {
        status: raw.status,
        pattern,
        priority: raw.priority,
        region: raw.region,
    })
}

/// Parses and validates a detection definition from an in-memory TOML
/// string.
///
/// This is a pure function: no filesystem access, no [`crate::DaemonConfig`]
/// defaults applied. Resource limits (source size, rule count, regex
/// compiled-program size) are enforced here since they are properties of
/// the parsed content itself, not of the storage medium.
///
/// # Errors
///
/// Returns [`DefinitionError`] for oversized input, malformed TOML, unknown
/// enum values, missing required keys, an unsupported `schema_version`, more
/// than [`MAX_RULES`] rules, or a rule pattern that fails to compile.
pub fn parse_definition(source: &str) -> Result<Definition, DefinitionError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(DefinitionError::SourceTooLarge {
            max: MAX_SOURCE_BYTES,
            actual: source.len(),
        });
    }

    let raw: RawDefinition = toml::from_str(source)?;

    if raw.schema_version != SUPPORTED_SCHEMA_VERSION {
        return Err(DefinitionError::UnsupportedSchemaVersion {
            expected: SUPPORTED_SCHEMA_VERSION,
            actual: raw.schema_version,
        });
    }

    if raw.rules.len() > MAX_RULES {
        return Err(DefinitionError::TooManyRules {
            max: MAX_RULES,
            actual: raw.rules.len(),
        });
    }

    let rules = raw
        .rules
        .into_iter()
        .map(compile_rule)
        .collect::<Result<Vec<_>, _>>()?;

    for key in raw
        .input
        .as_ref()
        .map(RawInputSection::written)
        .unwrap_or_default()
    {
        tracing::warn!(
            definition = raw.name.as_str(),
            key,
            "detection definition [input] {key} is deprecated and has no effect: the daemon no \
             longer types into windows; remove it"
        );
    }
    let retired_composer_keys = raw.composer.as_ref().map_or([false; 2], |composer| {
        [
            composer.placeholder.is_some(),
            composer.right_border.is_some(),
        ]
    });
    for (key, written) in ["placeholder", "right_border"]
        .into_iter()
        .zip(retired_composer_keys)
    {
        if written {
            tracing::warn!(
                definition = raw.name.as_str(),
                key,
                "detection definition [composer] {key} is deprecated and has no effect: the \
                 daemon no longer types into windows; remove it"
            );
        }
    }

    let composer = if let Some(raw_composer) = raw.composer {
        Some(compile_composer_section(raw_composer)?)
    } else {
        None
    };

    Ok(Definition {
        schema_version: raw.schema_version,
        name: raw.name,
        bottom_line_count: raw.bottom_line_count,
        debounce: raw.debounce,
        rules,
        quiescence: raw.quiescence,
        activity: raw.activity,
        composer,
        requests: raw.requests,
    })
}

/// Reads `path` and delegates to [`parse_definition`].
///
/// Path validity (build-artifact exclusion) is the caller's responsibility;
/// see `window::load_definition_for`.
///
/// # Errors
///
/// Returns [`DefinitionError::Io`] if `path` cannot be read, or any of the
/// [`parse_definition`] errors if it can be read but is rejected.
pub fn load_definition(path: &Path) -> Result<Definition, DefinitionError> {
    let source = std::fs::read_to_string(path).map_err(|source| DefinitionError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_definition(&source)
}

// Bundled definitions generated by `build.rs`.
include!(concat!(env!("OUT_DIR"), "/bundled_definitions.rs"));

/// The TOML source of the bundled definition `name`, or `None` when no
/// bundled definition has that name. Names are the file stems of this
/// crate's `detection/*.toml`, validated at build time. Never panics.
pub fn bundled_definition_source(name: &str) -> Option<&'static str> {
    BUNDLED_DEFINITIONS
        .binary_search_by_key(&name, |entry| entry.0)
        .ok()
        .map(|idx| BUNDLED_DEFINITIONS[idx].1)
}

/// Every bundled definition name, in byte order. Never empty (an empty
/// `detection/` directory fails the build). Never panics.
pub fn bundled_definition_names() -> &'static [&'static str] {
    BUNDLED_NAMES
}
