//! A profile's `[hook_events]` table: what the daemon does when a hook the
//! profile declared calls the `--hook <profile-id> <Event>` door.
//!
//! The table is data. The core reads it and acts on it; it never branches on
//! which CLI a profile is for.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::de::IntoDeserializer;
use thiserror::Error;
use yagura_protocol::WindowStatus;

/// The longest event name a table key, or a door argument, may be.
const MAX_EVENT_NAME_LEN: usize = 64;
/// The longest JSON pointer, in characters, an event may name.
const MAX_POINTER_CHARS: usize = 256;

/// What a hook's event asks the daemon to read from the window's inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDeliver {
    /// The room's introduction and what is waiting, left in the inbox.
    Context,
    /// What is waiting, taken out of the inbox.
    Take,
}

/// What the daemon does for one event. Applied in a fixed order:
/// `skip_if`, then `session`, then `status`, then `deliver`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEvent {
    /// The status the window is recorded in.
    pub status: Option<WindowStatus>,
    /// What the hook prints, in the `[delivery].output` form.
    pub deliver: Option<HookDeliver>,
    /// The JSON pointer to the session identifier in the hook's input.
    pub session: Option<String>,
    /// The JSON pointer that, when the hook's input has a value there, makes
    /// the event do nothing.
    pub skip_if: Option<String>,
}

/// A parsed and validated `[hook_events]` table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfileHookEvents {
    events: BTreeMap<String, HookEvent>,
}

impl ProfileHookEvents {
    /// The event this name selects, if the table has it.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&HookEvent> {
        self.events.get(name)
    }
}

/// Why a `[hook_events]` table was rejected. Carried only as the `source()`
/// of [`crate::profile::ProfileParseError::InvalidHookEvents`], so it reaches
/// the daemon's log without appearing in the fixed client-visible warning.
#[derive(Debug, Clone, Error)]
pub enum HookEventsError {
    /// `hook_events` is not a table.
    #[error("`hook_events` is not a table")]
    NotATable,
    /// A key is not an event name ([`is_hook_event_name`]).
    #[error("an event name is outside [A-Za-z0-9_-]{{1,64}}")]
    InvalidEventName,
    /// An event's value is not a table.
    #[error("an event's value is not a table")]
    EventNotATable,
    /// An event has a key nothing reads.
    #[error("an event has an unknown key {0:?}")]
    UnknownKey(String),
    /// The value of this key is of the wrong type or outside its domain.
    #[error("`{0}` is of the wrong type or outside its domain")]
    Invalid(&'static str),
    /// An event has none of `status`, `deliver` and `session`, so it would do
    /// nothing.
    #[error("an event has none of `status`, `deliver` and `session`")]
    NoAction,
}

/// Whether `name` is an event name: `[A-Za-z0-9_-]{1,64}`.
#[must_use]
pub fn is_hook_event_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_EVENT_NAME_LEN
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Reads and validates a profile's `hook_events` value. `None` in, `None` out.
pub(crate) fn parse_hook_events(
    raw: Option<toml::Value>,
) -> Result<Option<ProfileHookEvents>, HookEventsError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let toml::Value::Table(table) = raw else {
        return Err(HookEventsError::NotATable);
    };
    let mut events = BTreeMap::new();
    for (name, value) in table {
        if !is_hook_event_name(&name) {
            return Err(HookEventsError::InvalidEventName);
        }
        events.insert(name, parse_event(value)?);
    }
    Ok(Some(ProfileHookEvents { events }))
}

fn parse_event(value: toml::Value) -> Result<HookEvent, HookEventsError> {
    let toml::Value::Table(mut table) = value else {
        return Err(HookEventsError::EventNotATable);
    };
    let status = take_string(&mut table, "status")?
        .map(|value| parse_window_status(&value).ok_or(HookEventsError::Invalid("status")))
        .transpose()?;
    let deliver = match take_string(&mut table, "deliver")?.as_deref() {
        None => None,
        Some("context") => Some(HookDeliver::Context),
        Some("take") => Some(HookDeliver::Take),
        Some(_) => return Err(HookEventsError::Invalid("deliver")),
    };
    let session = take_pointer(&mut table, "session")?;
    let skip_if = take_pointer(&mut table, "skip_if")?;
    if let Some(key) = table.keys().next() {
        return Err(HookEventsError::UnknownKey(key.clone()));
    }
    if status.is_none() && deliver.is_none() && session.is_none() {
        return Err(HookEventsError::NoAction);
    }
    Ok(HookEvent {
        status,
        deliver,
        session,
        skip_if,
    })
}

/// Reads a status through `WindowStatus`'s own serde spelling, so the set a
/// hook event accepts and the set the `--emit-status` door accepts are one.
pub(crate) fn parse_window_status(value: &str) -> Option<WindowStatus> {
    let deserializer: serde::de::value::StrDeserializer<'_, serde::de::value::Error> =
        value.into_deserializer();
    WindowStatus::deserialize(deserializer).ok()
}

fn take_string(
    table: &mut toml::Table,
    key: &'static str,
) -> Result<Option<String>, HookEventsError> {
    match table.remove(key) {
        None => Ok(None),
        Some(toml::Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(HookEventsError::Invalid(key)),
    }
}

fn take_pointer(
    table: &mut toml::Table,
    key: &'static str,
) -> Result<Option<String>, HookEventsError> {
    let pointer = take_string(table, key)?;
    match pointer {
        Some(pointer)
            if !pointer.starts_with('/') || pointer.chars().count() > MAX_POINTER_CHARS =>
        {
            Err(HookEventsError::Invalid(key))
        }
        pointer => Ok(pointer),
    }
}
