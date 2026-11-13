//! How a window's CLI is stopped: a profile's `[interrupt]` table, and the
//! turn of the window's CLI an AI's interrupt stops.
//!
//! The table is data, as `[hook_events]` is: the core reads it and never
//! branches on which CLI a profile is for. The turn is daemon memory only,
//! moved by the facts the window's hooks report, never by its status or a
//! clock: the status is the detector's judgement, and a long tool with no
//! output can show `waiting_input` in the middle of a turn.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};

use thiserror::Error;
use tokio::sync::watch;

use crate::input_encode::NamedPress;
use crate::profile_hook_events::{ProfileHookEvents, is_hook_event_name};

/// The placeholder a stop answer carries the daemon's reason in.
const REASON_PLACEHOLDER: &str = "{reason}";
/// What a stop answer tells the CLI, as one JSON string.
const STOP_REASON: &str = "yagura: this window was asked to stop. Stop the current work and \
                           wait for the next instruction.";

/// A parsed and validated `[interrupt]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileInterrupt {
    /// The keys the user's own interrupt writes.
    pub user_keys: Vec<NamedPress>,
    /// How an AI stops the CLI.
    pub ai: AiMeans,
}

/// How an AI's interrupt stops a window's CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiMeans {
    /// Writing these keys.
    Keys(Vec<NamedPress>),
    /// Marking the CLI's open turn, whose next stop event is answered with a
    /// stop.
    Hook(HookStops),
    /// No means: an AI's interrupt is refused.
    None,
}

/// The hook form of an AI's interrupt: the events that start and end the
/// CLI's turn, and the events a stop is answered at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookStops {
    starts: BTreeSet<String>,
    ends: BTreeSet<String>,
    stops: BTreeMap<String, StopAnswer>,
}

/// What a hook event is to the CLI's turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnRole<'a> {
    /// It starts a turn.
    Start,
    /// It ends a turn.
    End,
    /// A stop is answered at it.
    Stop(&'a StopAnswer),
    /// None of these.
    Other,
}

impl HookStops {
    /// What `event` is to the turn.
    #[must_use]
    pub fn role(&self, event: &str) -> TurnRole<'_> {
        if self.starts.contains(event) {
            TurnRole::Start
        } else if self.ends.contains(event) {
            TurnRole::End
        } else if let Some(answer) = self.stops.get(event) {
            TurnRole::Stop(answer)
        } else {
            TurnRole::Other
        }
    }

    /// The events a stop is answered at.
    pub fn stop_events(&self) -> impl Iterator<Item = &str> {
        self.stops.keys().map(String::as_str)
    }
}

/// A stop event's answer: the JSON its hook prints, and whether that answer
/// ends the main session's turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopAnswer {
    template: String,
    ends_turn: bool,
}

impl StopAnswer {
    /// The answer's text, with the daemon's reason in it as one JSON string
    /// wherever the template has `{reason}`, as `[delivery]` output takes
    /// `{text}`.
    #[must_use]
    pub fn render(&self) -> String {
        render_template(&self.template)
    }

    /// Whether the answer, given to the main session, ends its turn: the
    /// tool does not run and the CLI waits for its user.
    #[must_use]
    pub fn ends_turn(&self) -> bool {
        self.ends_turn
    }
}

fn render_template(template: &str) -> String {
    let reason = serde_json::Value::String(STOP_REASON.to_string()).to_string();
    template.replace(REASON_PLACEHOLDER, &reason)
}

/// Why an `[interrupt]` table was rejected. Carried only as a `source()`, so
/// it reaches the daemon's log without appearing in the fixed client-visible
/// warning.
#[derive(Debug, Clone, Error)]
pub enum InterruptError {
    /// A value is of the wrong type or shape, or a key nothing reads is set.
    #[error("`{0}` is of the wrong type, outside its domain, or not read here")]
    Invalid(&'static str),
    /// A key is missing.
    #[error("`{0}` is missing")]
    Missing(&'static str),
    /// A key names a word outside the key vocabulary.
    #[error("a key word is not one of escape and ctrl_c")]
    UnknownKey,
    /// An event of the hook form is not in the profile's `[hook_events]`.
    #[error("an event the hook form names is not in [hook_events]")]
    EventNotDeclared,
    /// An event is in two of the start, end and stop lists.
    #[error("an event is in two of the start, end and stop lists")]
    EventInTwoLists,
    /// A stop answer has no `{reason}`, or is not JSON once it is filled in.
    #[error("a stop answer has no {{reason}} or is not JSON once filled in")]
    InvalidAnswer,
    /// A stop answer says it ends the main session's turn in a profile that
    /// tells no subagent apart, so a subagent's stop would end it too.
    #[error("ends_turn is true in a profile with no subagent_pointer")]
    EndsTurnWithoutSubagentPointer,
}

/// Reads the `[interrupt]` table's structure: what it says, before the events
/// it names are checked against `[hook_events]` ([`check_against_events`]).
/// `None` in, `None` out.
///
/// # Errors
///
/// Returns the rule the table breaks.
pub(crate) fn parse_interrupt(
    raw: Option<toml::Value>,
) -> Result<Option<ProfileInterrupt>, InterruptError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let toml::Value::Table(mut table) = raw else {
        return Err(InterruptError::Invalid("interrupt"));
    };
    let user_keys =
        parse_keys(table.remove("user_keys"))?.ok_or(InterruptError::Missing("user_keys"))?;
    let ai = match table.remove("ai") {
        Some(toml::Value::String(ai)) => ai,
        Some(_) => return Err(InterruptError::Invalid("ai")),
        None => return Err(InterruptError::Missing("ai")),
    };
    let ai_keys = parse_keys(table.remove("ai_keys"))?;
    let turn = table.remove("turn");
    let stops = table.remove("stops");
    if let Some(key) = table.keys().next() {
        tracing::debug!(key = %key, "[interrupt] has a key nothing reads");
        return Err(InterruptError::Invalid("interrupt"));
    }
    let ai = match ai.as_str() {
        "keys" => {
            if turn.is_some() || stops.is_some() {
                return Err(InterruptError::Invalid("turn"));
            }
            AiMeans::Keys(ai_keys.ok_or(InterruptError::Missing("ai_keys"))?)
        }
        "hook" => {
            if ai_keys.is_some() {
                return Err(InterruptError::Invalid("ai_keys"));
            }
            AiMeans::Hook(parse_hook_stops(
                turn.ok_or(InterruptError::Missing("turn"))?,
                stops.ok_or(InterruptError::Missing("stops"))?,
            )?)
        }
        "none" => {
            if ai_keys.is_some() || turn.is_some() || stops.is_some() {
                return Err(InterruptError::Invalid("ai"));
            }
            AiMeans::None
        }
        _ => return Err(InterruptError::Invalid("ai")),
    };
    Ok(Some(ProfileInterrupt { user_keys, ai }))
}

/// Checks the events the hook form names against the profile's
/// `[hook_events]`: each must be declared there, so its door reaches the
/// daemon, and a stop answer may end the main turn only where a subagent's
/// hook can be told apart.
///
/// # Errors
///
/// Returns the rule the table breaks.
pub(crate) fn check_against_events(
    interrupt: &ProfileInterrupt,
    events: Option<&ProfileHookEvents>,
) -> Result<(), InterruptError> {
    let AiMeans::Hook(hook) = &interrupt.ai else {
        return Ok(());
    };
    let Some(events) = events else {
        return Err(InterruptError::EventNotDeclared);
    };
    let named = hook
        .starts
        .iter()
        .chain(&hook.ends)
        .chain(hook.stops.keys());
    for event in named {
        if events.get(event).is_none() {
            return Err(InterruptError::EventNotDeclared);
        }
    }
    if events.subagent_pointer().is_none() && hook.stops.values().any(StopAnswer::ends_turn) {
        return Err(InterruptError::EndsTurnWithoutSubagentPointer);
    }
    Ok(())
}

/// The names of the stop events a raw `[interrupt]` table declares, read
/// before `[hook_events]` is, which takes an event declared only to be
/// stopped at.
pub(crate) fn stop_event_names(interrupt: Option<&ProfileInterrupt>) -> BTreeSet<String> {
    match interrupt.map(|interrupt| &interrupt.ai) {
        Some(AiMeans::Hook(hook)) => hook.stop_events().map(str::to_string).collect(),
        _ => BTreeSet::new(),
    }
}

fn parse_keys(raw: Option<toml::Value>) -> Result<Option<Vec<NamedPress>>, InterruptError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let toml::Value::Array(words) = raw else {
        return Err(InterruptError::Invalid("keys"));
    };
    if words.is_empty() {
        return Err(InterruptError::Invalid("keys"));
    }
    words
        .into_iter()
        .map(|word| match word {
            toml::Value::String(word) => {
                NamedPress::from_word(&word).ok_or(InterruptError::UnknownKey)
            }
            _ => Err(InterruptError::Invalid("keys")),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn parse_hook_stops(turn: toml::Value, stops: toml::Value) -> Result<HookStops, InterruptError> {
    let toml::Value::Table(mut turn) = turn else {
        return Err(InterruptError::Invalid("turn"));
    };
    let starts = parse_events(turn.remove("starts"), "starts")?;
    let ends = parse_events(turn.remove("ends"), "ends")?;
    if !turn.is_empty() {
        return Err(InterruptError::Invalid("turn"));
    }
    let toml::Value::Table(stops) = stops else {
        return Err(InterruptError::Invalid("stops"));
    };
    if stops.is_empty() {
        return Err(InterruptError::Missing("stops"));
    }
    let stops = stops
        .into_iter()
        .map(|(event, answer)| {
            if !is_hook_event_name(&event) {
                return Err(InterruptError::Invalid("stops"));
            }
            Ok((event, parse_stop_answer(answer)?))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let overlaps = starts.intersection(&ends).next().is_some()
        || stops
            .keys()
            .any(|event| starts.contains(event) || ends.contains(event));
    if overlaps {
        return Err(InterruptError::EventInTwoLists);
    }
    Ok(HookStops {
        starts,
        ends,
        stops,
    })
}

/// A turn's start or end events: a non-empty list of event names.
fn parse_events(
    raw: Option<toml::Value>,
    key: &'static str,
) -> Result<BTreeSet<String>, InterruptError> {
    let Some(toml::Value::Array(events)) = raw else {
        return Err(InterruptError::Missing(key));
    };
    if events.is_empty() {
        return Err(InterruptError::Missing(key));
    }
    events
        .into_iter()
        .map(|event| match event {
            toml::Value::String(event) if is_hook_event_name(&event) => Ok(event),
            _ => Err(InterruptError::Invalid(key)),
        })
        .collect()
}

fn parse_stop_answer(raw: toml::Value) -> Result<StopAnswer, InterruptError> {
    let toml::Value::Table(mut table) = raw else {
        return Err(InterruptError::Invalid("stops"));
    };
    let template = match table.remove("answer") {
        Some(toml::Value::String(template)) => template,
        Some(_) => return Err(InterruptError::Invalid("answer")),
        None => return Err(InterruptError::Missing("answer")),
    };
    let ends_turn = match table.remove("ends_turn") {
        Some(toml::Value::Boolean(ends_turn)) => ends_turn,
        Some(_) => return Err(InterruptError::Invalid("ends_turn")),
        None => return Err(InterruptError::Missing("ends_turn")),
    };
    if !table.is_empty() {
        return Err(InterruptError::Invalid("stops"));
    }
    let filled = render_template(&template);
    if !template.contains(REASON_PLACEHOLDER)
        || serde_json::from_str::<serde_json::Value>(&filled).is_err()
    {
        return Err(InterruptError::InvalidAnswer);
    }
    Ok(StopAnswer {
        template,
        ends_turn,
    })
}

/// Where a window's CLI is in its turns, as its main hooks told it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Phase {
    /// No main hook has reached the daemon since the window opened, was taken
    /// back, or the daemon started: whether its CLI has hooks at all is not
    /// known.
    #[default]
    Unobserved,
    /// A turn is open.
    Open,
    /// The last turn ended and no other has started.
    Closed,
}

#[derive(Debug, Default)]
struct TurnState {
    phase: Phase,
    /// Counts the turns started; a mark names the turn it was put on.
    number: u64,
    mark: Option<u64>,
}

impl TurnState {
    fn marked(&self) -> bool {
        self.mark == Some(self.number)
    }
}

/// What putting a mark on a window's turn came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marked {
    /// The open turn is marked.
    Open,
    /// The turn is marked, but no main hook has reached the daemon.
    Unobserved,
    /// The last turn ended: nothing was marked.
    NoOpenTurn,
}

/// A window's turns and the mark an AI's interrupt puts on one: daemon memory
/// only, which a new window, a taken-back one and a restarted daemon start
/// without.
#[derive(Debug)]
pub struct TurnLedger {
    state: Mutex<TurnState>,
    /// Bumped each time what the window is waiting for is to end: a mark
    /// newly put, or an interrupt by keys.
    stops: watch::Sender<u64>,
}

impl Default for TurnLedger {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            stops: watch::channel(0).0,
        }
    }
}

impl TurnLedger {
    fn state(&self) -> std::sync::MutexGuard<'_, TurnState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A main hook of `role` reached the daemon: a start opens a new turn, an
    /// end closes it, and any other main hook shows a turn is under way when
    /// none was seen yet.
    pub fn main_hook(&self, role: TurnRole<'_>) {
        let mut state = self.state();
        match role {
            TurnRole::Start => {
                state.number += 1;
                state.phase = Phase::Open;
            }
            TurnRole::End => state.phase = Phase::Closed,
            TurnRole::Stop(_) | TurnRole::Other => {
                if state.phase == Phase::Unobserved {
                    state.phase = Phase::Open;
                }
            }
        }
    }

    /// Whether a stop event is answered with a stop now: the turn it belongs
    /// to is the marked one, and it has not ended.
    #[must_use]
    pub fn stops_now(&self) -> bool {
        let state = self.state();
        state.phase != Phase::Closed && state.marked()
    }

    /// The main session was given a stop answer that ends its turn.
    pub fn stop_ended_turn(&self) {
        self.state().phase = Phase::Closed;
    }

    /// A stop's block was handed over: the CLI goes on with it in a new turn.
    pub fn block_started(&self) {
        let mut state = self.state();
        state.number += 1;
        state.phase = Phase::Open;
    }

    /// A waiting door was handed a parcel, which wakes its CLI: a new turn,
    /// unless one is open. Returns the turn it started.
    #[must_use]
    pub fn wake_started(&self) -> Option<u64> {
        let mut state = self.state();
        if state.phase == Phase::Open {
            return None;
        }
        state.number += 1;
        state.phase = Phase::Open;
        Some(state.number)
    }

    /// The parcel that started turn `number` came back unprinted: the CLI was
    /// never woken, so that turn is closed again, unless another began since.
    pub fn wake_unprinted(&self, number: u64) {
        let mut state = self.state();
        if state.phase == Phase::Open && state.number == number {
            state.phase = Phase::Closed;
        }
    }

    /// Puts an AI's mark on the turn under way. A turn marked already is left
    /// as it is: asking again is the same request.
    pub fn mark(&self) -> Marked {
        let mut state = self.state();
        let marked = match state.phase {
            Phase::Closed => return Marked::NoOpenTurn,
            Phase::Open => Marked::Open,
            Phase::Unobserved => Marked::Unobserved,
        };
        if !state.marked() {
            state.mark = Some(state.number);
            drop(state);
            self.stops.send_modify(|count| *count += 1);
        }
        marked
    }

    /// The window's CLI was interrupted by its keys, which ends at once what
    /// the window was waiting for.
    pub fn interrupted_by_keys(&self) {
        self.stops.send_modify(|count| *count += 1);
    }

    /// Changes each time what the window is waiting for is to end.
    #[must_use]
    pub fn subscribe_stops(&self) -> watch::Receiver<u64> {
        self.stops.subscribe()
    }

    /// Whether a request the window starts now ends at once: its open turn,
    /// one a main hook confirmed, carries the mark.
    #[must_use]
    pub fn ends_requests_now(&self) -> bool {
        let state = self.state();
        state.phase == Phase::Open && state.marked()
    }
}
