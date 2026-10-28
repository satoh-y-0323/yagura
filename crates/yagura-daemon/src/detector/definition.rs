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
/// `blank_lines_in_body` is optional and defaults to `false`; `left_panel`,
/// `right_border` and `placeholder` are optional and have no default.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
    /// A regex pattern string; see [`ComposerSection::right_border`].
    #[serde(default)]
    pub right_border: Option<String>,
    /// A regex pattern string; see [`ComposerSection::placeholder`].
    #[serde(default)]
    pub placeholder: Option<String>,
}

/// The compiled section of a definition that identifies the composer (input area)
/// on the screen, allowing the daemon to read what has been typed.
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
    /// A compiled regex matching an edge the CLI draws at the right end of
    /// each composer row (a box's right border), so the edge can be told
    /// from typed text. On the marker row and every continuation row, a
    /// match ending at the row's last visible character is dropped before
    /// the row is read, before anything right of the cursor is judged.
    pub right_border: Option<Regex>,
    /// A compiled regex matching a hint the CLI draws in an empty composer
    /// without dimming it. The composer reads as empty only when nothing is
    /// typed left of the cursor on the marker row and one match spans that
    /// row's whole visible text right of the cursor; any other hint reads as
    /// occupied.
    pub placeholder: Option<Regex>,
}

/// Raw values from a definition's optional `[input]` table: how the CLI this
/// definition describes expects a line of text to be written and submitted.
///
/// Every field mirrors a TOML key literally and stays `None` when the
/// definition omits it, or when the whole `[input]` table is absent: this
/// loader applies no default for any of them (see the module doc), so a
/// caller that owns the defaults can tell "not written" from "written as the
/// default value". The values that do load are range- and
/// consistency-checked; see [`DefinitionError::InvalidInput`].
///
/// # Examples
///
/// ```
/// use yagura_daemon::detector::definition::parse_definition;
///
/// let toml = r#"
/// schema_version = 1
/// name = "example"
///
/// [debounce]
/// confirm_ms = 300
///
/// [[rules]]
/// status = "idle"
/// pattern = '\$\s*$'
/// priority = 1
/// region = "bottom"
///
/// [input]
/// submit = "\r"
/// "#;
///
/// let definition = parse_definition(toml).expect("a valid definition must load");
/// let input = definition.input.expect("the definition declares [input]");
/// assert_eq!(input.submit.as_deref(), Some("\r"));
/// assert_eq!(input.bracketed_paste, None);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputSection {
    /// Whether the body should be wrapped in `ESC[200~` / `ESC[201~` before
    /// being written.
    pub bracketed_paste: Option<bool>,
    /// The bytes that submit a line, written verbatim (1..=8 bytes).
    pub submit: Option<String>,
    /// How long the confirmed status must have stayed in a status that
    /// accepts input, once, before this window counts as past its startup
    /// phase (0..=10000; 0 = count it as past from the moment the window is
    /// created). A prompt a CLI paints while it is still starting up is
    /// indistinguishable from one it paints when it is ready, so the daemon
    /// asks the prompt to *stay*.
    pub ready_hold_ms: Option<u64>,
    /// Milliseconds to wait after writing the body before writing `submit`
    /// (0..=5000).
    pub submit_delay_ms: Option<u64>,
    /// One key that takes text back out of the receiver's editor and may be
    /// repeated until it is empty: it is written once per display line the
    /// text may occupy, and on an empty editor it does nothing (0..=8 bytes;
    /// empty means "send nothing").
    pub clear: Option<String>,
    /// Milliseconds to wait for a delivery reaction after `submit` is
    /// written before giving up (0..=30000; `0` disables the wait).
    pub delivery_confirm_ms: Option<u64>,
    /// How many times a submit may be retried after an unconfirmed delivery
    /// (0..=4; `0` means never retry).
    pub resend_max: Option<u8>,
    /// Accepted so a definition written for an older daemon still loads, and
    /// has no effect: whether a write showed is decided by events, never by a
    /// bound on how long to wait for it.
    pub reflect_wait_ms: Option<u64>,
    /// One printable, non-blank character the receiver's editor shows as
    /// typed and that triggers nothing else there, written after a take-back
    /// to learn that everything before it was handled. Omitted, a take-back is
    /// never confirmed, so the window assumes something may remain.
    pub probe: Option<String>,
    /// The key that erases one character (1..=8 bytes; default DEL).
    pub erase: Option<String>,
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
    /// How this CLI's input is written to, if the definition's `[input]`
    /// table is present. See [`InputSection`].
    pub input: Option<InputSection>,
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
    /// An `[input]` value is out of range, or two `[input]` (or `[input]`
    /// and `[debounce]`) values contradict each other. `key` is always the
    /// offending `input.*` key (or `input.*` paired with `debounce.*`), so
    /// the rendered message names which key to fix.
    #[error("input.{key} {reason}")]
    InvalidInput {
        /// The `[input]` key this rejection is about (without the `input.`
        /// prefix, which the message adds).
        key: String,
        /// Why `key` was rejected, completing the sentence after `input.{key} `.
        reason: String,
    },
}

/// Convenience constructor for [`DefinitionError::InvalidInput`].
fn invalid_input(key: &str, reason: impl Into<String>) -> DefinitionError {
    DefinitionError::InvalidInput {
        key: key.to_string(),
        reason: reason.into(),
    }
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
    input: Option<InputSection>,
    #[serde(default)]
    requests: RequestsSection,
}

/// Upper bound (inclusive) on `[input].ready_hold_ms`, in milliseconds.
const MAX_READY_HOLD_MS: u64 = 10_000;

/// Upper bound (inclusive) on `[input].submit_delay_ms`, in milliseconds.
const MAX_SUBMIT_DELAY_MS: u64 = 5000;

/// Upper bound (inclusive) on `[input].delivery_confirm_ms`, in
/// milliseconds.
const MAX_DELIVERY_CONFIRM_MS: u64 = 30_000;

/// The first character of every line the daemon writes on its own (`[yagura `
/// handoff, task, plan, chat and report lines), which `[input].probe` must
/// not be.
const DAEMON_LINE_LEAD: &str = "[";

/// Upper bound (inclusive) on `[input].resend_max`.
const MAX_RESEND_MAX: u8 = 4;

/// Upper bound (inclusive), in bytes, on `[input].submit` and
/// `[input].clear`.
const MAX_INPUT_SEQUENCE_BYTES: usize = 8;

/// The `[input].submit_delay_ms` value a definition that omits the key runs
/// under. `InputProfile::from_section` (a later layer, outside this module)
/// applies this same default to the value it hands the daemon; this loader
/// never writes it onto [`InputSection`] (see the module doc), but the
/// "submit delay must outlast the debounce" consistency check below judges
/// the value a definition will actually run under, so it reads this
/// constant when the key is omitted.
pub const INPUT_SUBMIT_DELAY_DEFAULT_MS: u64 = 800;

/// Where the `confirm_ms` value [`check_submit_delay_outlasts`] judged
/// `submit_delay_ms` against came from, so the rejection message can name
/// the key a caller should change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmMsSource {
    /// The definition's own `[debounce].confirm_ms`.
    Debounce,
    /// `DaemonConfig::confirm_ms`, used because the definition (or the
    /// caller's window) has no `[debounce]` table to read instead.
    DaemonDefault,
}

/// The single comparison the delivery-confirmation defense at submit time
/// depends on: the body's own echo must settle into a confirmed status
/// *before* the submit key is written, which requires `submit_delay_ms` to
/// outlast the `confirm_ms` a window actually debounces under. Called both
/// by [`validate_input_section`]
/// (when the definition declares its own `[debounce]`) and by
/// `WindowRegistry::initialize_detection_for_window` (when it does not, and
/// the effective value is the daemon's own default instead) so the two call
/// sites never drift apart.
///
/// # Errors
///
/// Returns [`DefinitionError::InvalidInput`] naming `submit_delay_ms` when
/// `submit_delay_ms <= effective_confirm_ms`.
pub fn check_submit_delay_outlasts(
    submit_delay_ms: u64,
    effective_confirm_ms: u64,
    source: ConfirmMsSource,
) -> Result<(), DefinitionError> {
    if submit_delay_ms > effective_confirm_ms {
        return Ok(());
    }
    let reason = match source {
        ConfirmMsSource::Debounce => "must be greater than debounce.confirm_ms".to_string(),
        ConfirmMsSource::DaemonDefault => format!(
            "must be greater than the daemon's default confirm_ms ({effective_confirm_ms}ms, \
             since this definition has no [debounce] table)"
        ),
    };
    Err(invalid_input("submit_delay_ms", reason))
}

/// Range- and consistency-checks a definition's `[input]` table against the
/// same definition's `[debounce]` table (`None` when the definition omits
/// it).
///
/// Every field is optional and independently range-checked when present. An
/// omitted `submit_delay_ms` is judged against [`INPUT_SUBMIT_DELAY_DEFAULT_MS`],
/// the value such a definition actually runs under, rather than left
/// unjudged.
fn validate_input_section(
    section: &InputSection,
    debounce: Option<&Debounce>,
) -> Result<(), DefinitionError> {
    if let Some(value) = section.ready_hold_ms
        && value > MAX_READY_HOLD_MS
    {
        return Err(invalid_input(
            "ready_hold_ms",
            format!("must be between 0 and {MAX_READY_HOLD_MS}, got {value}"),
        ));
    }
    if let Some(value) = section.submit_delay_ms
        && value > MAX_SUBMIT_DELAY_MS
    {
        return Err(invalid_input(
            "submit_delay_ms",
            format!("must be between 0 and {MAX_SUBMIT_DELAY_MS}, got {value}"),
        ));
    }
    if let Some(value) = section.delivery_confirm_ms
        && value > MAX_DELIVERY_CONFIRM_MS
    {
        return Err(invalid_input(
            "delivery_confirm_ms",
            format!("must be between 0 and {MAX_DELIVERY_CONFIRM_MS}, got {value}"),
        ));
    }
    if let Some(value) = section.resend_max
        && value > MAX_RESEND_MAX
    {
        return Err(invalid_input(
            "resend_max",
            format!("must be between 0 and {MAX_RESEND_MAX}, got {value}"),
        ));
    }
    if let Some(probe) = section.probe.as_deref() {
        let mut chars = probe.chars();
        let one_printable = match (chars.next(), chars.next()) {
            (Some(c), None) => !c.is_control() && !c.is_whitespace(),
            _ => false,
        };
        if !one_printable {
            return Err(invalid_input(
                "probe",
                "must be exactly one printable, non-blank character",
            ));
        }
        // A probe that is the first character of the daemon's own lines would
        // read alone exactly as one of those lines begins to show.
        if probe == DAEMON_LINE_LEAD {
            return Err(invalid_input(
                "probe",
                format!(
                    "must not be {DAEMON_LINE_LEAD:?}, the first character of every line the \
                     daemon writes"
                ),
            ));
        }
    }
    if let Some(erase) = section.erase.as_deref()
        && (erase.is_empty() || erase.len() > MAX_INPUT_SEQUENCE_BYTES)
    {
        return Err(invalid_input(
            "erase",
            format!("must be 1 to {MAX_INPUT_SEQUENCE_BYTES} bytes"),
        ));
    }
    if let Some(submit) = section.submit.as_deref() {
        if submit.is_empty() {
            return Err(invalid_input("submit", "must not be empty"));
        }
        if submit.len() > MAX_INPUT_SEQUENCE_BYTES {
            return Err(invalid_input(
                "submit",
                format!(
                    "must be at most {MAX_INPUT_SEQUENCE_BYTES} bytes, got {}",
                    submit.len()
                ),
            ));
        }
    }
    if let Some(clear) = section.clear.as_deref()
        && clear.len() > MAX_INPUT_SEQUENCE_BYTES
    {
        return Err(invalid_input(
            "clear",
            format!(
                "must be at most {MAX_INPUT_SEQUENCE_BYTES} bytes, got {}",
                clear.len()
            ),
        ));
    }
    let resend_requested = section.resend_max.is_some_and(|value| value > 0);
    if section.delivery_confirm_ms == Some(0) && resend_requested {
        return Err(invalid_input(
            "resend_max",
            "requires input.delivery_confirm_ms to be non-zero",
        ));
    }
    // An omitted submit_delay_ms is judged against the default it actually
    // runs under (INPUT_SUBMIT_DELAY_DEFAULT_MS), not left unjudged: a
    // missing [debounce] is the one side this loader truly cannot know
    // (DaemonConfig's confirm_ms, which this loader never reads -- see the
    // module doc), so only that side skips the check.
    if let Some(debounce) = debounce {
        let submit_delay_ms = section
            .submit_delay_ms
            .unwrap_or(INPUT_SUBMIT_DELAY_DEFAULT_MS);
        if section.delivery_confirm_ms != Some(0) {
            check_submit_delay_outlasts(
                submit_delay_ms,
                debounce.confirm_ms,
                ConfirmMsSource::Debounce,
            )?;
        }
    }

    Ok(())
}

fn compile_composer_section(raw: ComposerRawSection) -> Result<ComposerSection, DefinitionError> {
    let marker = compile_composer_pattern(raw.marker)?;
    let left_panel = raw.left_panel.map(compile_composer_pattern).transpose()?;
    let right_border = raw.right_border.map(compile_composer_pattern).transpose()?;
    let placeholder = raw.placeholder.map(compile_composer_pattern).transpose()?;

    Ok(ComposerSection {
        marker,
        separator: raw.separator,
        continuation_prefix: raw.continuation_prefix,
        blank_lines_in_body: raw.blank_lines_in_body,
        left_panel,
        right_border,
        placeholder,
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
/// than [`MAX_RULES`] rules, a rule pattern that fails to compile, or an
/// `[input]` table with an out-of-range or self-contradictory value (see
/// [`DefinitionError::InvalidInput`]).
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

    if let Some(input) = &raw.input {
        validate_input_section(input, raw.debounce.as_ref())?;
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
        input: raw.input,
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
