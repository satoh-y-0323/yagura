//! A profile's `[delivery]` table: how the daemon hands text to that profile's
//! CLI without typing it into the composer.
//!
//! The table is data. The core reads it and acts on it; it never branches on
//! which CLI a profile is for.

use thiserror::Error;
use yagura_protocol::{DeliveryStream, ProfileGuidance};

use crate::inbox::{DEFAULT_MAX_CHARS, DEFAULT_WAKE_EXIT, DeliverySpec, DoorOutputs, OutputForm};
use crate::profile::contains_placeholder_outside;
use crate::window::{CliInjection, InjectionKind};

/// Where an output template, and a wake command, take the text.
const TEXT_PLACEHOLDER: &str = "{text}";
/// Where a wake command takes the CLI's session identifier.
const SESSION_PLACEHOLDER: &str = "{session}";
/// Where a launch prompt template takes the prompt.
const PROMPT_PLACEHOLDER: &str = "{prompt}";

/// How the daemon wakes a CLI that is waiting for its user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeMode {
    /// There is no way to: what is waiting is read at the next turn.
    None,
    /// The CLI leaves a wait registered with the daemon (`--deliver-wait`),
    /// which the daemon answers when something arrives.
    Wait,
    /// The daemon runs the profile's own program with these arguments, where
    /// `{text}` and `{session}` are filled in.
    Command(Vec<String>),
}

/// A parsed and validated `[delivery]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileDelivery {
    /// How the doors print the text they carry.
    pub output: DoorOutputs,
    /// The most characters one answer carries, the intro included.
    pub max_chars: usize,
    /// How a waiting CLI is woken.
    pub wake: WakeMode,
    /// The stream a wait that was answered prints on.
    pub wake_stream: DeliveryStream,
    /// The exit code that wait ends with.
    pub wake_exit: u8,
    /// The arguments that hand the CLI a first request at start-up, where
    /// `{prompt}` is filled in.
    pub launch_prompt: Option<Vec<String>>,
}

impl ProfileDelivery {
    /// How a window of this profile answers at its doors.
    #[must_use]
    pub fn spec(&self) -> DeliverySpec {
        DeliverySpec {
            max_chars: self.max_chars,
            output: self.output.clone(),
            wake_stream: self.wake_stream,
            wake_exit: self.wake_exit,
        }
    }
}

/// Why a `[delivery]` table was rejected. Carried only as the `source()` of
/// [`crate::profile::ProfileParseError::InvalidDelivery`], so it reaches the
/// daemon's log without appearing in the fixed client-visible warning.
#[derive(Debug, Clone, Error)]
pub enum DeliveryError {
    /// `delivery` is not a table.
    #[error("`delivery` is not a table")]
    NotATable,
    /// The table has a key nothing reads.
    #[error("the table has an unknown key {0:?}")]
    UnknownKey(String),
    /// The value of this key is of the wrong type or outside its domain.
    #[error("`{0}` is of the wrong type or outside its domain")]
    Invalid(&'static str),
}

/// Reads and validates a profile's `delivery` value. `None` in, `None` out.
pub(crate) fn parse_delivery(
    raw: Option<toml::Value>,
) -> Result<Option<ProfileDelivery>, DeliveryError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let toml::Value::Table(mut table) = raw else {
        return Err(DeliveryError::NotATable);
    };

    let output = parse_door_outputs(table.remove("output"))?;
    let max_chars = match take_integer(&mut table, "max_chars")? {
        None => DEFAULT_MAX_CHARS,
        Some(value) => usize::try_from(value)
            .ok()
            .filter(|chars| *chars >= 1)
            .ok_or(DeliveryError::Invalid("max_chars"))?,
    };
    let wake = parse_wake(
        take_string(&mut table, "wake")?,
        take_strings(&mut table, "wake_args")?,
    )?;
    let wake_stream = match take_string(&mut table, "wake_stream")?.as_deref() {
        None | Some("stderr") => DeliveryStream::Stderr,
        Some("stdout") => DeliveryStream::Stdout,
        Some(_) => return Err(DeliveryError::Invalid("wake_stream")),
    };
    let wake_exit = match take_integer(&mut table, "wake_exit")? {
        None => DEFAULT_WAKE_EXIT,
        Some(value) => u8::try_from(value).map_err(|_| DeliveryError::Invalid("wake_exit"))?,
    };
    let launch_prompt = match take_strings(&mut table, "launch_prompt")? {
        None => None,
        Some(args) => Some(
            validated_args(args, &[PROMPT_PLACEHOLDER], PROMPT_PLACEHOLDER)
                .ok_or(DeliveryError::Invalid("launch_prompt"))?,
        ),
    };
    if let Some((key, _)) = table.into_iter().next() {
        return Err(DeliveryError::UnknownKey(key));
    }
    Ok(Some(ProfileDelivery {
        output,
        max_chars,
        wake,
        wake_stream,
        wake_exit,
        launch_prompt,
    }))
}

/// `{ text, command, url }`: `text` is required, the others optional. Each is
/// one line of visible text, and `url` is an `https` address, because a client
/// offers it to be opened. `None` when `raw` breaks any of these rules.
pub(crate) fn parse_guidance(raw: toml::Value) -> Option<ProfileGuidance> {
    let toml::Value::Table(mut table) = raw else {
        return None;
    };
    let mut take_line = |key: &'static str| -> Option<Option<String>> {
        match take_string(&mut table, key).ok()? {
            Some(line) if !is_visible_line(&line) => None,
            other => Some(other),
        }
    };
    let text = take_line("text")??;
    let command = take_line("command")?;
    let url = take_line("url")?;
    if url.as_deref().is_some_and(|url| !is_https_url(url)) || !table.is_empty() {
        return None;
    }
    Some(ProfileGuidance { text, command, url })
}

/// Some visible character, and no control character a reader would act on.
pub(crate) fn is_visible_line(line: &str) -> bool {
    !line.trim().is_empty() && !line.chars().any(char::is_control)
}

/// `https://` followed by a host, with no whitespace anywhere.
fn is_https_url(url: &str) -> bool {
    url.strip_prefix("https://")
        .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
        && !url.chars().any(char::is_whitespace)
}

/// `output` is one string (every door prints the same way) or a table keyed
/// `context` / `take` / `wait`; a door the table leaves out prints plain.
fn parse_door_outputs(raw: Option<toml::Value>) -> Result<DoorOutputs, DeliveryError> {
    let invalid = || DeliveryError::Invalid("output");
    match raw {
        None => Ok(DoorOutputs::all(&OutputForm::Plain)),
        Some(toml::Value::String(text)) => {
            Ok(DoorOutputs::all(&parse_output(text).ok_or_else(invalid)?))
        }
        Some(toml::Value::Table(mut table)) => {
            let mut door =
                |key: &'static str| match take_string(&mut table, key).map_err(|_| invalid())? {
                    None => Ok(OutputForm::Plain),
                    Some(text) => parse_output(text).ok_or_else(invalid),
                };
            let outputs = DoorOutputs {
                context: door("context")?,
                take: door("take")?,
                wait: door("wait")?,
            };
            if !table.is_empty() {
                return Err(invalid());
            }
            Ok(outputs)
        }
        Some(_) => Err(invalid()),
    }
}

/// `plain`, or a JSON template that is still JSON once the text is in it.
fn parse_output(text: String) -> Option<OutputForm> {
    if text == "plain" {
        return Some(OutputForm::Plain);
    }
    if !text.contains(TEXT_PLACEHOLDER) || contains_placeholder_outside(&text, &[TEXT_PLACEHOLDER])
    {
        return None;
    }
    let template = OutputForm::Template(text);
    serde_json::from_str::<serde_json::Value>(&template.render("x")).ok()?;
    Some(template)
}

fn parse_wake(mode: Option<String>, args: Option<Vec<String>>) -> Result<WakeMode, DeliveryError> {
    let invalid = || DeliveryError::Invalid("wake");
    match (mode.as_deref(), args) {
        (None | Some("none"), None) => Ok(WakeMode::None),
        (Some("wait"), None) => Ok(WakeMode::Wait),
        (Some("command"), Some(args)) => validated_args(
            args,
            &[TEXT_PLACEHOLDER, SESSION_PLACEHOLDER],
            TEXT_PLACEHOLDER,
        )
        .map(WakeMode::Command)
        .ok_or_else(invalid),
        _ => Err(invalid()),
    }
}

/// `args` when no element is empty, none spells a slot outside `known`, and
/// one of them carries `required`.
fn validated_args(args: Vec<String>, known: &[&str], required: &str) -> Option<Vec<String>> {
    let valid = yagura_protocol::validate_argv_elements(&args).is_ok()
        && args
            .iter()
            .all(|arg| !contains_placeholder_outside(arg, known))
        && args.iter().any(|arg| arg.contains(required));
    valid.then_some(args)
}

fn take_string(
    table: &mut toml::Table,
    key: &'static str,
) -> Result<Option<String>, DeliveryError> {
    match table.remove(key) {
        None => Ok(None),
        Some(toml::Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(DeliveryError::Invalid(key)),
    }
}

fn take_integer(table: &mut toml::Table, key: &'static str) -> Result<Option<i64>, DeliveryError> {
    match table.remove(key) {
        None => Ok(None),
        Some(toml::Value::Integer(value)) => Ok(Some(value)),
        Some(_) => Err(DeliveryError::Invalid(key)),
    }
}

fn take_strings(
    table: &mut toml::Table,
    key: &'static str,
) -> Result<Option<Vec<String>>, DeliveryError> {
    match table.remove(key) {
        None => Ok(None),
        Some(toml::Value::Array(items)) => items
            .into_iter()
            .map(|item| match item {
                toml::Value::String(value) => Ok(value),
                _ => Err(DeliveryError::Invalid(key)),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(DeliveryError::Invalid(key)),
    }
}

/// The injection that hands a CLI `prompt` as its first request, or `None`
/// when the profile declares no way to. The prompt is put into each element of
/// the template once: whatever it holds stays as written.
#[must_use]
pub fn launch_prompt_injection(delivery: &ProfileDelivery, prompt: &str) -> Option<CliInjection> {
    let template = delivery.launch_prompt.as_ref()?;
    Some(CliInjection {
        kind: InjectionKind::LaunchPrompt,
        args: template
            .iter()
            .map(|arg| arg.replace(PROMPT_PLACEHOLDER, prompt))
            .collect(),
        config_file: None,
        config_dir: None,
    })
}
