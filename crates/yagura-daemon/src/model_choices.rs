//! Reading a listing command's standard output into model choices, and the
//! one-line reasons given when a listing cannot be read.
//!
//! Pure functions only: nothing here spawns, reads files or logs.

use std::collections::HashSet;

use serde_json::Value;
use thiserror::Error;
use yagura_protocol::{ModelChoice, validate_launch_option_value, validate_window_name};

use crate::profile::{JsonFilter, OutputFormat};

/// The most choices one listing may report. A listing with more is refused
/// rather than truncated, so a partial list is never shown as complete.
pub const MAX_CHOICES: usize = 256;

/// Why a profile's model choices could not be listed. Every `Display` is an
/// English fixed phrase that names no path and quotes nothing from the
/// command's output or standard error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChoicesFailure {
    /// The program could not be started, or the launcher reported it
    /// missing. `program` is a file name only, never a path.
    #[error("{program} was not found")]
    NotFound {
        /// The file name of the program that was not found.
        program: String,
    },
    /// The listing command cannot be started; the reason text is shown as
    /// given.
    #[error("{0}")]
    Unavailable(String),
    /// The command ran past its time limit and was stopped.
    #[error("did not finish within {secs} seconds")]
    TimedOut {
        /// The time limit, in seconds.
        secs: u64,
    },
    /// The command exited with a non-zero status other than "not found".
    #[error("exited with status {0}")]
    ExitStatus(i32),
    /// The command ended without reporting any exit status (for example,
    /// it was terminated by a signal).
    #[error("terminated without reporting an exit code")]
    ExitedWithoutCode,
    /// The command wrote more standard output than accepted.
    #[error("output too large")]
    OutputTooLarge,
    /// The output could not be read as the profile's format, or yielded no
    /// choice at all.
    #[error("output did not match the profile's format")]
    FormatMismatch,
    /// The output lists more than [`MAX_CHOICES`] choices.
    #[error("lists more models than accepted")]
    TooMany,
    /// The profile identifier names no listed launch profile.
    #[error("unknown launch profile")]
    UnknownProfile,
    /// The profile has no model option to list choices for.
    #[error("this profile does not map the model option")]
    NoModelOption,
}

/// The exit status a POSIX shell reports for a command it could not find.
/// Shared with [`crate::mcp_registration`], whose status reading treats the
/// same exit code as "the CLI itself is not installed".
pub(crate) const COMMAND_NOT_FOUND_STATUS: i32 = 127;

/// Splits `text` into lines the way this crate reads a command's standard
/// output line by line: on `\n`, with a `\r` a CRLF source leaves at the end
/// of each line stripped. Shared by [`parse_choices`]'s `lines` format and
/// [`crate::mcp_registration`]'s status reading.
pub(crate) fn output_lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
}

/// Parses `text` as a JSON document, `None` when it is not one. Shared by
/// [`parse_choices`]'s `json` format and [`crate::mcp_registration`]'s status
/// reading.
pub(crate) fn parse_json_document(text: &str) -> Option<Value> {
    serde_json::from_str(text).ok()
}

/// Reads a listing command's standard output as `format` says.
///
/// An identifier must satisfy [`validate_launch_option_value`]; a line or
/// element whose identifier does not is dropped. A display name must
/// satisfy [`validate_window_name`]; one that does not is dropped and the
/// identifier kept alone. A repeated identifier keeps its first occurrence.
///
/// # Errors
///
/// Returns [`ChoicesFailure::FormatMismatch`] when `stdout` is not UTF-8,
/// is not JSON for a `json` format, the `items` pointer does not point at an
/// array, or no choice survives; and [`ChoicesFailure::TooMany`] when more
/// than [`MAX_CHOICES`] choices survive.
pub fn parse_choices(
    format: &OutputFormat,
    stdout: &[u8],
) -> Result<Vec<ModelChoice>, ChoicesFailure> {
    let text = std::str::from_utf8(stdout).map_err(|_| ChoicesFailure::FormatMismatch)?;
    let mut collector = Collector::default();
    match format {
        OutputFormat::Lines { separator } => {
            for line in output_lines(text) {
                if line.is_empty() {
                    continue;
                }
                let (id, label) = match line.split_once(separator.as_str()) {
                    Some((id, label)) => (id, Some(label)),
                    None => (line, None),
                };
                collector.push(id, label)?;
            }
        }
        OutputFormat::Json {
            items,
            id,
            label,
            filter,
        } => {
            let document = parse_json_document(text).ok_or(ChoicesFailure::FormatMismatch)?;
            let elements = document
                .pointer(items)
                .and_then(Value::as_array)
                .ok_or(ChoicesFailure::FormatMismatch)?;
            for element in elements {
                if !passes_filter(element, filter.as_ref()) {
                    continue;
                }
                let Some(choice_id) = element.pointer(id).and_then(Value::as_str) else {
                    continue;
                };
                let choice_label = label
                    .as_deref()
                    .and_then(|pointer| element.pointer(pointer))
                    .and_then(Value::as_str);
                collector.push(choice_id, choice_label)?;
            }
        }
    }
    collector.finish()
}

/// Builds the reason for a listing command that exited unsuccessfully.
///
/// Status 127 is the shell's "command not found", so it names the last
/// element of `launch_argv`: behind a launcher prefix that is the program
/// the shell could not find. Only its file name is shown, never a
/// directory.
pub fn failure_for_exit(code: Option<i32>, launch_argv: &[String]) -> ChoicesFailure {
    match code {
        Some(COMMAND_NOT_FOUND_STATUS) => ChoicesFailure::NotFound {
            program: file_name(launch_argv.last()),
        },
        Some(status) => ChoicesFailure::ExitStatus(status),
        None => ChoicesFailure::ExitedWithoutCode,
    }
}

/// Builds the reason for a listing command that failed to start.
///
/// Every spawn failure reads as "not found" naming the file name of
/// `argv[0]`; `argv` is the unresolved argv, so the name is the one the
/// profile wrote. `err` is not shown: its text may carry a path.
pub fn failure_for_spawn_error(err: &std::io::Error, argv: &[String]) -> ChoicesFailure {
    let _ = err;
    ChoicesFailure::NotFound {
        program: file_name(argv.first()),
    }
}

/// The last path component of `element`, splitting on both separators so a
/// Windows path read on any host still loses its directory.
fn file_name(element: Option<&String>) -> String {
    element
        .and_then(|element| element.rsplit(['/', '\\']).next())
        .unwrap_or_default()
        .to_string()
}

fn passes_filter(element: &Value, filter: Option<&JsonFilter>) -> bool {
    filter.is_none_or(|filter| {
        element.pointer(&filter.pointer).and_then(Value::as_str) == Some(filter.equals.as_str())
    })
}

/// Accumulates accepted choices, enforcing the identifier and label rules,
/// first-occurrence de-duplication and the count limit.
#[derive(Default)]
struct Collector {
    choices: Vec<ModelChoice>,
    seen: HashSet<String>,
}

impl Collector {
    fn push(&mut self, id: &str, label: Option<&str>) -> Result<(), ChoicesFailure> {
        if validate_launch_option_value(id).is_err() || self.seen.contains(id) {
            return Ok(());
        }
        if self.choices.len() == MAX_CHOICES {
            return Err(ChoicesFailure::TooMany);
        }
        self.seen.insert(id.to_string());
        self.choices.push(ModelChoice {
            id: id.to_string(),
            label: label
                .filter(|label| validate_window_name(label).is_ok())
                .map(str::to_string),
        });
        Ok(())
    }

    fn finish(self) -> Result<Vec<ModelChoice>, ChoicesFailure> {
        if self.choices.is_empty() {
            return Err(ChoicesFailure::FormatMismatch);
        }
        Ok(self.choices)
    }
}
