//! The ticket rules a user configures in `tickets.toml` (the board's guide,
//! its states in column order, the fields a state requires), and the bounds
//! on ticket and milestone text.
//!
//! Like [`crate::plan`], this is a pure validation layer: it knows neither the
//! store nor any window. The only behaviour a state carries is its required
//! fields and its position; no state name is special.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::config_file::ConfigFileError;
use yagura_protocol::{
    MILESTONE_DESCRIPTION_MAX_BYTES, MultilineTextError, TICKET_BODY_MAX_BYTES,
    TICKET_FIELD_VALUE_MAX_BYTES, TICKET_RESULT_MAX_BYTES, WindowNameError, is_valid_token,
    validate_display_label, validate_multiline_text,
};

/// Largest `tickets.toml` [`read_ticket_config`] reads, in bytes.
pub const MAX_TICKET_CONFIG_BYTES: usize = crate::config_file::MAX_CONFIG_FILE_BYTES;

/// Most `[[state]]` entries a `tickets.toml` may declare.
pub const MAX_TICKET_STATES: usize = 32;

/// Most `[[field]]` entries a `tickets.toml` may declare.
pub const MAX_TICKET_FIELDS: usize = 32;

/// Schema version [`read_ticket_config`] accepts.
const SUPPORTED_SCHEMA: u32 = 1;

/// A validated `tickets.toml`: at least one state, every name a unique
/// token, and every `requires` entry naming a declared field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketConfig {
    /// The board's rules as the user wrote them.
    pub guide: String,
    /// States in file order, which is the board's column order.
    pub states: Vec<TicketState>,
    /// Fields in file order.
    pub fields: Vec<TicketField>,
}

/// One `[[state]]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketState {
    /// State name, an [`is_valid_token`].
    pub name: String,
    /// What the state means, as the user wrote it.
    pub description: String,
    /// Fields that must hold a non-blank value while a ticket is in this
    /// state, in the order the user listed them.
    pub requires: Vec<String>,
}

/// One `[[field]]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketField {
    /// Field name, an [`is_valid_token`].
    pub name: String,
    /// What the field holds, as the user wrote it.
    pub description: String,
}

impl TicketConfig {
    /// The state a ticket starts in when none is given: the first
    /// `[[state]]`. [`read_ticket_config`] never returns a config without one.
    #[must_use]
    pub fn default_state(&self) -> &str {
        self.states.first().map_or("", |state| state.name.as_str())
    }

    /// The state named `name`, if this config declares it.
    #[must_use]
    pub fn state(&self, name: &str) -> Option<&TicketState> {
        self.states.iter().find(|state| state.name == name)
    }

    /// The field named `name`, if this config declares it.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&TicketField> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// The fields `state` requires that are absent, empty or whitespace-only
    /// in `fields`, in the order the state lists them.
    ///
    /// A state this config does not declare requires nothing; whether the
    /// state name itself is acceptable is [`TicketConfig::state`]'s question.
    #[must_use]
    pub fn missing_required(&self, state: &str, fields: &BTreeMap<String, String>) -> Vec<String> {
        let Some(state) = self.state(state) else {
            return Vec::new();
        };
        state
            .requires
            .iter()
            .filter(|name| {
                fields
                    .get(*name)
                    .is_none_or(|value| value.trim().is_empty())
            })
            .cloned()
            .collect()
    }
}

/// Why [`read_ticket_config`] rejected a `tickets.toml`.
///
/// Every message is fixed: nothing from the file's content is echoed, since
/// the reason is shown to clients that did not write the file.
#[derive(Debug, Error)]
pub enum TicketConfigError {
    /// The file exists but could not be read.
    #[error("tickets.toml could not be read")]
    Io(#[source] std::io::Error),
    /// The file is larger than [`MAX_TICKET_CONFIG_BYTES`].
    #[error("tickets.toml exceeds the {max}-byte limit")]
    TooLarge {
        /// The limit that was exceeded.
        max: usize,
    },
    /// The file is not UTF-8.
    #[error("tickets.toml is not valid UTF-8")]
    NotUtf8,
    /// The file is not TOML of the expected shape (including an unknown
    /// key). The parser's own message is dropped because it quotes the file.
    #[error("tickets.toml could not be parsed{}", line_suffix(*line))]
    Toml {
        /// 1-based line the parser stopped at, when it reported one.
        line: Option<usize>,
    },
    /// `schema` is not the supported version.
    #[error("tickets.toml has an unsupported schema version")]
    UnsupportedSchema,
    /// No `[[state]]` is declared.
    #[error("tickets.toml declares no state")]
    NoStates,
    /// More than [`MAX_TICKET_STATES`] states.
    #[error("tickets.toml declares {actual} states, exceeding the {max}-state limit")]
    TooManyStates {
        /// The limit that was exceeded.
        max: usize,
        /// The declared count.
        actual: usize,
    },
    /// More than [`MAX_TICKET_FIELDS`] fields.
    #[error("tickets.toml declares {actual} fields, exceeding the {max}-field limit")]
    TooManyFields {
        /// The limit that was exceeded.
        max: usize,
        /// The declared count.
        actual: usize,
    },
    /// A state name is not an [`is_valid_token`].
    #[error("a state name in tickets.toml is not 1 to 64 characters of [a-z0-9_-]")]
    InvalidStateName,
    /// A field name is not an [`is_valid_token`].
    #[error("a field name in tickets.toml is not 1 to 64 characters of [a-z0-9_-]")]
    InvalidFieldName,
    /// Two states share a name.
    #[error("a state name in tickets.toml is declared twice")]
    DuplicateStateName,
    /// Two fields share a name.
    #[error("a field name in tickets.toml is declared twice")]
    DuplicateFieldName,
    /// A state's `requires` names a field no `[[field]]` declares.
    #[error("a state in tickets.toml requires a field that is not declared")]
    UnknownRequiredField,
    /// A state's `requires` names the same field twice.
    #[error("a state in tickets.toml requires the same field twice")]
    DuplicateRequiredField,
    /// The guide or a description holds a control or bidirectional-control
    /// character other than a newline or a tab.
    #[error("the guide or a description in tickets.toml contains a forbidden character")]
    ForbiddenChar,
}

fn line_suffix(line: Option<usize>) -> String {
    line.map(|line| format!(" (line {line})"))
        .unwrap_or_default()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTicketConfig {
    schema: u32,
    guide: String,
    #[serde(default, rename = "state")]
    states: Vec<RawState>,
    #[serde(default, rename = "field")]
    fields: Vec<RawField>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawState {
    name: String,
    description: String,
    #[serde(default)]
    requires: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawField {
    name: String,
    description: String,
}

/// Reads and validates `tickets.toml` at `path`.
///
/// `Ok(None)` means the file does not exist (the board is not configured);
/// a file that exists but is unusable is an error, never a guessed default.
/// The file is read again on every call, so an edit applies to the next
/// request without restarting the daemon.
///
/// # Errors
///
/// Returns [`TicketConfigError`] for a file that cannot be read, is larger
/// than [`MAX_TICKET_CONFIG_BYTES`], is not UTF-8 or TOML of the expected
/// shape, or breaks one of the rules its variants list. Any single violation
/// rejects the whole file.
pub fn read_ticket_config(path: &Path) -> Result<Option<TicketConfig>, TicketConfigError> {
    let bytes = match crate::config_file::read_config_file(path) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(None),
        Err(ConfigFileError::Io(err)) => return Err(TicketConfigError::Io(err)),
        Err(ConfigFileError::TooLarge) => {
            return Err(TicketConfigError::TooLarge {
                max: MAX_TICKET_CONFIG_BYTES,
            });
        }
    };
    let text = String::from_utf8(bytes).map_err(|_| TicketConfigError::NotUtf8)?;
    let raw: RawTicketConfig = toml::from_str(&text).map_err(|err| TicketConfigError::Toml {
        line: err.span().map(|span| line_of(&text, span.start)),
    })?;
    validate(raw).map(Some)
}

fn line_of(text: &str, offset: usize) -> usize {
    let end = offset.min(text.len());
    text.as_bytes()[..end]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1
}

fn validate(raw: RawTicketConfig) -> Result<TicketConfig, TicketConfigError> {
    if raw.schema != SUPPORTED_SCHEMA {
        return Err(TicketConfigError::UnsupportedSchema);
    }
    if raw.states.is_empty() {
        return Err(TicketConfigError::NoStates);
    }
    if raw.states.len() > MAX_TICKET_STATES {
        return Err(TicketConfigError::TooManyStates {
            max: MAX_TICKET_STATES,
            actual: raw.states.len(),
        });
    }
    if raw.fields.len() > MAX_TICKET_FIELDS {
        return Err(TicketConfigError::TooManyFields {
            max: MAX_TICKET_FIELDS,
            actual: raw.fields.len(),
        });
    }
    if !is_config_text(&raw.guide) {
        return Err(TicketConfigError::ForbiddenChar);
    }

    let mut field_names: HashSet<&str> = HashSet::with_capacity(raw.fields.len());
    for field in &raw.fields {
        if !is_valid_token(&field.name) {
            return Err(TicketConfigError::InvalidFieldName);
        }
        if !field_names.insert(field.name.as_str()) {
            return Err(TicketConfigError::DuplicateFieldName);
        }
        if !is_config_text(&field.description) {
            return Err(TicketConfigError::ForbiddenChar);
        }
    }

    let mut state_names: HashSet<&str> = HashSet::with_capacity(raw.states.len());
    for state in &raw.states {
        if !is_valid_token(&state.name) {
            return Err(TicketConfigError::InvalidStateName);
        }
        if !state_names.insert(state.name.as_str()) {
            return Err(TicketConfigError::DuplicateStateName);
        }
        if !is_config_text(&state.description) {
            return Err(TicketConfigError::ForbiddenChar);
        }
        let mut required: HashSet<&str> = HashSet::with_capacity(state.requires.len());
        for name in &state.requires {
            if !field_names.contains(name.as_str()) {
                return Err(TicketConfigError::UnknownRequiredField);
            }
            if !required.insert(name.as_str()) {
                return Err(TicketConfigError::DuplicateRequiredField);
            }
        }
    }

    Ok(TicketConfig {
        guide: raw.guide,
        states: raw
            .states
            .into_iter()
            .map(|state| TicketState {
                name: state.name,
                description: state.description,
                requires: state.requires,
            })
            .collect(),
        fields: raw
            .fields
            .into_iter()
            .map(|field| TicketField {
                name: field.name,
                description: field.description,
            })
            .collect(),
    })
}

/// The guide and descriptions are shown to windows and in the GUI, so they
/// follow the multi-line text rule; empty is allowed.
fn is_config_text(text: &str) -> bool {
    text.is_empty() || validate_multiline_text(text, MAX_TICKET_CONFIG_BYTES).is_ok()
}

/// Why a ticket or milestone text value was rejected. No variant echoes the
/// value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TicketTextError {
    /// A one-line label (a ticket or milestone title) broke the label rule.
    #[error(transparent)]
    Label(#[from] WindowNameError),
    /// A multi-line value broke the multi-line text rule.
    #[error(transparent)]
    Text(#[from] MultilineTextError),
    /// A milestone name is not an [`is_valid_token`].
    #[error("milestone name must be 1 to 64 characters of [a-z0-9_-]")]
    InvalidMilestoneName,
}

/// Validates a ticket's title: one line, the display-label rule.
///
/// # Errors
///
/// Returns [`TicketTextError::Label`] when [`validate_display_label`] rejects it.
pub fn validate_ticket_title(value: &str) -> Result<(), TicketTextError> {
    Ok(validate_display_label(value)?)
}

/// Validates a ticket's body: multi-line, at most [`TICKET_BODY_MAX_BYTES`].
///
/// # Errors
///
/// Returns [`TicketTextError::Text`] when [`validate_multiline_text`] rejects it.
pub fn validate_ticket_body(value: &str) -> Result<(), TicketTextError> {
    Ok(validate_multiline_text(value, TICKET_BODY_MAX_BYTES)?)
}

/// Validates one field value: multi-line, at most
/// [`TICKET_FIELD_VALUE_MAX_BYTES`]. Clearing a field with `""` is the
/// caller's decision before this check.
///
/// # Errors
///
/// Returns [`TicketTextError::Text`] when [`validate_multiline_text`] rejects it.
pub fn validate_ticket_field_value(value: &str) -> Result<(), TicketTextError> {
    Ok(validate_multiline_text(
        value,
        TICKET_FIELD_VALUE_MAX_BYTES,
    )?)
}

/// Validates one result: multi-line, at most [`TICKET_RESULT_MAX_BYTES`].
///
/// # Errors
///
/// Returns [`TicketTextError::Text`] when [`validate_multiline_text`] rejects it.
pub fn validate_ticket_result(value: &str) -> Result<(), TicketTextError> {
    Ok(validate_multiline_text(value, TICKET_RESULT_MAX_BYTES)?)
}

/// Validates a milestone name: an [`is_valid_token`].
///
/// # Errors
///
/// Returns [`TicketTextError::InvalidMilestoneName`] otherwise.
pub fn validate_milestone_name(value: &str) -> Result<(), TicketTextError> {
    if is_valid_token(value) {
        Ok(())
    } else {
        Err(TicketTextError::InvalidMilestoneName)
    }
}

/// Validates a milestone's title: one line, the display-label rule.
///
/// # Errors
///
/// Returns [`TicketTextError::Label`] when [`validate_display_label`] rejects it.
pub fn validate_milestone_title(value: &str) -> Result<(), TicketTextError> {
    Ok(validate_display_label(value)?)
}

/// Validates a milestone's description: multi-line, at most
/// [`MILESTONE_DESCRIPTION_MAX_BYTES`].
///
/// # Errors
///
/// Returns [`TicketTextError::Text`] when [`validate_multiline_text`] rejects it.
pub fn validate_milestone_description(value: &str) -> Result<(), TicketTextError> {
    Ok(validate_multiline_text(
        value,
        MILESTONE_DESCRIPTION_MAX_BYTES,
    )?)
}
