//! A profile's `[hook_events]` table: what the daemon does when a hook the
//! profile declared calls the `--hook <profile-id> <Event>` door.
//!
//! The table is data. The core reads it and acts on it; it never branches on
//! which CLI a profile is for.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde::de::IntoDeserializer;
use thiserror::Error;
use yagura_protocol::WindowStatus;

/// The longest event name a table key, or a door argument, may be.
pub(crate) const MAX_EVENT_NAME_LEN: usize = 64;
/// The longest JSON pointer, in characters, an event may name.
const MAX_POINTER_CHARS: usize = 256;
/// The top-level profile key naming the pointer that tells a subagent's hook
/// apart ([`ProfileHookEvents::subagent_pointer`]).
pub(crate) const SUBAGENT_POINTER_KEY: &str = "subagent_pointer";
/// The per-event key [`SUBAGENT_POINTER_KEY`] replaced, still read once.
const OLDER_SKIP_IF_KEY: &str = "skip_if";

/// Whether a hook came from the window's main session or from a subagent the
/// session started: a fact of the CLI's hook input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOrigin {
    /// The window's own session.
    Main,
    /// A subagent of it, foreground or background.
    Subagent,
}

impl HookOrigin {
    /// The origin of a hook whose input has (`true`) or lacks a value at the
    /// subagent pointer.
    #[must_use]
    pub fn from_subagent_value(present: bool) -> Self {
        if present { Self::Subagent } else { Self::Main }
    }
}

/// What a hook can do to its window. Which of these a subagent's hook does is
/// yagura's decision, made once in [`HookEffect::applies_to`]; a profile only
/// says how its CLI marks a subagent's hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEffect {
    /// Recording this status.
    Status(WindowStatus),
    /// Recording the session the hook names.
    Session,
    /// Handing the inbox over: a take, the context, a wait or a block.
    Deliver,
    /// Moving the window's turn: a hook reached the daemon, and a turn
    /// started or ended (`[interrupt]`'s hook form).
    Turn,
    /// Answering a stop at a stop event of the marked turn.
    Stop,
}

impl HookEffect {
    /// Whether a hook of `origin` has this effect.
    #[must_use]
    pub fn applies_to(self, origin: HookOrigin) -> bool {
        match (self, origin) {
            (_, HookOrigin::Main) => true,
            // A subagent's work is the window's work, a background subagent's included.
            (
                Self::Status(WindowStatus::Running | WindowStatus::WaitingApproval),
                HookOrigin::Subagent,
            ) => true,
            // A subagent's end is not the end of the window's turn.
            (Self::Status(_), HookOrigin::Subagent) => false,
            // The session and the inbox are the main session's.
            (Self::Session | Self::Deliver, HookOrigin::Subagent) => false,
            // A subagent's start or end is not the window's turn's.
            (Self::Turn, HookOrigin::Subagent) => false,
            // A subagent works in the window's turn, so it stops with it.
            (Self::Stop, HookOrigin::Subagent) => true,
        }
    }
}

/// What a hook's event asks the daemon to read from the window's inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDeliver {
    /// The room's introduction and what is waiting, left in the inbox.
    Context,
    /// What is waiting, taken out of the inbox.
    Take,
    /// What is waiting, taken out of the inbox as the block of a stop, which
    /// makes the CLI go on with it. The event's `status` is recorded where its
    /// door ends instead: at once when nothing is handed over, `running` once
    /// the door says it printed the block, and the event's own when the read
    /// ends without saying so and no status was reported since it began.
    Block,
}

/// What the daemon does for one event. Applied in a fixed order: `session`,
/// then the window's turn (an `[interrupt]` hook form's start or end), then
/// `status`, then a stop when the event is a stop event of the marked turn
/// (answered in place of `deliver`), then `deliver` (which `when` gates; a
/// block moves `status` to where its door ends, see [`HookDeliver::Block`]),
/// each only when it [`HookEffect::applies_to`] the hook's [`HookOrigin`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEvent {
    /// The status the window is recorded in.
    pub status: Option<WindowStatus>,
    /// What the hook prints, in the `[delivery].output` form.
    pub deliver: Option<HookDeliver>,
    /// The JSON pointer to the session identifier in the hook's input.
    pub session: Option<String>,
    /// The values the hook's input must hold, at these JSON pointers, for
    /// `deliver` to hand anything over. The door compares them and tells the
    /// daemon only whether each one matched; a value that is missing does not.
    pub when: BTreeMap<String, serde_json::Value>,
}

/// A parsed and validated `[hook_events]` table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfileHookEvents {
    events: BTreeMap<String, HookEvent>,
    subagent: Option<String>,
}

impl ProfileHookEvents {
    /// The event this name selects, if the table has it.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&HookEvent> {
        self.events.get(name)
    }

    /// The JSON pointer at which a subagent's hook input, on every event, has
    /// a value and the main session's has none. `None` when the profile names
    /// none: every hook is the main session's.
    #[must_use]
    pub fn subagent_pointer(&self) -> Option<&str> {
        self.subagent.as_deref()
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
    /// The events' older `skip_if` values differ, so no one subagent pointer
    /// keeps what each event told apart.
    #[error("the events' `skip_if` values differ")]
    SkipIfDiffers,
    /// The `[interrupt]` table breaks its rule, which its hook form reads
    /// against this table.
    #[error("the interrupt table breaks its rule")]
    Interrupt(#[source] crate::profile_interrupt::InterruptError),
}

impl HookEventsError {
    /// The fixed phrase a profile listing shows for this error. Only the one
    /// that asks for a rewrite names the key to write; none names anything
    /// read from the document.
    #[must_use]
    pub(crate) fn listed_reason(&self) -> &'static str {
        match self {
            Self::SkipIfDiffers => {
                "invalid hook events table: the events' skip_if differ; name one subagent_pointer instead"
            }
            Self::Interrupt(_) => "invalid interrupt table",
            _ => "invalid hook events table",
        }
    }
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

/// Reads and validates a profile's `hook_events` value with its top-level
/// [`SUBAGENT_POINTER_KEY`]. `None` in, `None` out. An event named in
/// `stop_only` may do nothing else: `[interrupt]` answers a stop at it.
///
/// An older per-event `skip_if` becomes the subagent pointer when every event
/// that writes it writes the same value, with a warning naming the key to
/// write instead; values that differ are refused, since one pointer would
/// change how some event tells a subagent apart.
pub(crate) fn parse_hook_events(
    raw: Option<toml::Value>,
    raw_subagent: Option<toml::Value>,
    stop_only: &BTreeSet<String>,
    warnings: &mut Vec<String>,
) -> Result<Option<ProfileHookEvents>, HookEventsError> {
    let subagent = raw_subagent
        .map(|value| match value {
            toml::Value::String(pointer) if is_pointer(&pointer) => Ok(pointer),
            _ => Err(HookEventsError::Invalid(SUBAGENT_POINTER_KEY)),
        })
        .transpose()?;
    let Some(raw) = raw else {
        if subagent.is_some() {
            warnings.push(format!(
                "{SUBAGENT_POINTER_KEY} is read only with a [hook_events] table and was ignored"
            ));
        }
        return Ok(None);
    };
    let toml::Value::Table(table) = raw else {
        return Err(HookEventsError::NotATable);
    };
    let mut events = BTreeMap::new();
    let mut older = BTreeSet::new();
    for (name, value) in table {
        if !is_hook_event_name(&name) {
            return Err(HookEventsError::InvalidEventName);
        }
        let (event, skip_if) = parse_event(value, stop_only.contains(&name))?;
        older.extend(skip_if);
        events.insert(name, event);
    }
    let subagent = match (subagent, older.len()) {
        (subagent, 0) => subagent,
        (Some(subagent), _) => {
            warnings.push(format!(
                "[hook_events] {OLDER_SKIP_IF_KEY} was ignored, as {SUBAGENT_POINTER_KEY} is \
                 given; remove {OLDER_SKIP_IF_KEY}"
            ));
            Some(subagent)
        }
        (None, 1) => {
            warnings.push(format!(
                "[hook_events] {OLDER_SKIP_IF_KEY} is read as {SUBAGENT_POINTER_KEY}, which \
                 tells a subagent's hook apart on every event, those without {OLDER_SKIP_IF_KEY} \
                 too; write {SUBAGENT_POINTER_KEY} instead"
            ));
            older.pop_first()
        }
        (None, _) => return Err(HookEventsError::SkipIfDiffers),
    };
    Ok(Some(ProfileHookEvents { events, subagent }))
}

/// One event and the older `skip_if` it wrote, if any.
fn parse_event(
    value: toml::Value,
    stop_only: bool,
) -> Result<(HookEvent, Option<String>), HookEventsError> {
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
        Some("block") => Some(HookDeliver::Block),
        Some(_) => return Err(HookEventsError::Invalid("deliver")),
    };
    let session = take_pointer(&mut table, "session")?;
    let skip_if = take_pointer(&mut table, OLDER_SKIP_IF_KEY)?;
    let when = parse_when(table.remove("when"))?;
    if let Some(key) = table.keys().next() {
        return Err(HookEventsError::UnknownKey(key.clone()));
    }
    if status.is_none() && deliver.is_none() && session.is_none() && !stop_only {
        return Err(HookEventsError::NoAction);
    }
    if !when.is_empty() && deliver.is_none() {
        return Err(HookEventsError::Invalid("when"));
    }
    let event = HookEvent {
        status,
        deliver,
        session,
        when,
    };
    Ok((event, skip_if))
}

/// `when`: a table of JSON pointers to a string, an integer or a boolean.
fn parse_when(
    raw: Option<toml::Value>,
) -> Result<BTreeMap<String, serde_json::Value>, HookEventsError> {
    let invalid = || HookEventsError::Invalid("when");
    let Some(raw) = raw else {
        return Ok(BTreeMap::new());
    };
    let toml::Value::Table(table) = raw else {
        return Err(invalid());
    };
    table
        .into_iter()
        .map(|(pointer, value)| {
            if !is_pointer(&pointer) {
                return Err(invalid());
            }
            let value = match value {
                toml::Value::String(text) => serde_json::Value::String(text),
                toml::Value::Integer(number) => number.into(),
                toml::Value::Boolean(flag) => flag.into(),
                _ => return Err(invalid()),
            };
            Ok((pointer, value))
        })
        .collect()
}

/// Whether `pointer` is a JSON pointer an event may name.
fn is_pointer(pointer: &str) -> bool {
    pointer.starts_with('/') && pointer.chars().count() <= MAX_POINTER_CHARS
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
        Some(pointer) if !is_pointer(&pointer) => Err(HookEventsError::Invalid(key)),
        pointer => Ok(pointer),
    }
}
