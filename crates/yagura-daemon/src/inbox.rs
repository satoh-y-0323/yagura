//! A window's inbox: the parcels the daemon holds for the window's CLI to read
//! through the doors its hooks run (`deliver_context`, `deliver_take`,
//! `deliver_wait`).
//!
//! The daemon never types a parcel into the window. The CLI asks for it: at
//! start-up, at the start of a turn, or by leaving a wait registered that the
//! daemon answers when a parcel arrives.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::{oneshot, watch};
use yagura_protocol::{DaemonMessage, DeliveryStream, WindowStatus};

use crate::waiter::StatusAt;

/// Between two parcels, and between the intro and the first parcel.
const SEPARATOR: &str = "\n\n";

/// Where a template takes the text.
const TEXT_PLACEHOLDER: &str = "{text}";

/// Default value of [`DeliverySpec::max_chars`].
pub const DEFAULT_MAX_CHARS: usize = 10_000;

/// Default value of [`DeliverySpec::wake_exit`].
pub const DEFAULT_WAKE_EXIT: u8 = 2;

/// The most characters a session identifier has.
const SESSION_ID_MAX_CHARS: usize = 128;

/// Whether `value` has the shape of a session identifier a window's CLI
/// reported: 1 to 128 ASCII letters, digits and `-_.:`, not starting with `-`.
#[must_use]
pub fn is_session_id(value: &str) -> bool {
    let length = value.chars().count();
    (1..=SESSION_ID_MAX_CHARS).contains(&length)
        && !value.starts_with('-')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

/// The string at `pointer` (a JSON pointer, RFC 6901) in `input`, a hook's
/// stdin or the start of it.
///
/// `input` may stop in the middle of the document: whatever precedes the cut is
/// still read, so a value that sits within the first bytes is found however long
/// the rest is. `None` when `input` is not JSON up to the value, nothing is
/// at the pointer, or what is there is not a string.
#[must_use]
pub fn session_at_pointer(input: &[u8], pointer: &str) -> Option<String> {
    let path: Vec<String> = pointer
        .strip_prefix('/')?
        .split('/')
        .map(|token| token.replace("~1", "/").replace("~0", "~"))
        .collect();
    let mut found = None;
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    // Reading stops at the value, or fails at the cut; either way the answer
    // is what `found` holds.
    let _ = serde::de::DeserializeSeed::deserialize(
        PointerSeek {
            path: &path,
            found: &mut found,
        },
        &mut deserializer,
    );
    found
}

/// Walks a JSON document along a pointer, recording the string it ends at.
struct PointerSeek<'a> {
    path: &'a [String],
    found: &'a mut Option<String>,
}

impl<'de> serde::de::DeserializeSeed<'de> for PointerSeek<'_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for PointerSeek<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON document")
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<(), E> {
        if self.path.is_empty() {
            *self.found = Some(value.to_string());
        }
        Ok(())
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        use serde::de::{Error, IgnoredAny};
        let Some((token, rest)) = self.path.split_first() else {
            return Ok(());
        };
        while let Some(key) = map.next_key::<String>()? {
            if key == *token {
                map.next_value_seed(PointerSeek {
                    path: rest,
                    found: self.found,
                })?;
                // Looked at: the rest of the document is not read.
                return Err(A::Error::custom("pointer reached"));
            }
            map.next_value::<IgnoredAny>()?;
        }
        Ok(())
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        use serde::de::{Error, IgnoredAny};
        let Some((token, rest)) = self.path.split_first() else {
            return Ok(());
        };
        let Ok(index) = token.parse::<usize>() else {
            return Ok(());
        };
        for position in 0.. {
            if position == index {
                seq.next_element_seed(PointerSeek {
                    path: rest,
                    found: self.found,
                })?;
                return Err(A::Error::custom("pointer reached"));
            }
            if seq.next_element::<IgnoredAny>()?.is_none() {
                break;
            }
        }
        Ok(())
    }
}

/// One piece of text for a window's CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parcel {
    /// Names the parcel to whoever deposited it, so a parcel turned away can
    /// be told apart from the ones that stayed.
    pub key: String,
    /// What the CLI reads.
    pub text: String,
}

impl Parcel {
    /// A parcel of `text` named `key`.
    #[must_use]
    pub fn new(key: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            text: text.into(),
        }
    }
}

/// How a door prints the text it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputForm {
    /// The text as it is.
    Plain,
    /// A JSON document with the text embedded as one JSON string wherever
    /// the template has `{text}`.
    Template(String),
}

impl OutputForm {
    /// The output for `text`; nothing at all when there is no text.
    ///
    /// The text is inserted in a single pass, so a `{text}` inside it is
    /// never replaced.
    #[must_use]
    pub fn render(&self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        match self {
            Self::Plain => text.to_string(),
            Self::Template(template) => {
                let embedded = serde_json::Value::String(text.to_string()).to_string();
                template
                    .split(TEXT_PLACEHOLDER)
                    .collect::<Vec<_>>()
                    .join(&embedded)
            }
        }
    }
}

/// How a window's doors answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliverySpec {
    /// The most characters (`chars()`, the intro included) one answer
    /// carries.
    pub max_chars: usize,
    /// How the text is printed.
    pub output: OutputForm,
    /// The stream a wait that was answered by a parcel prints on.
    pub wake_stream: DeliveryStream,
    /// The exit code that wait ends with.
    pub wake_exit: u8,
}

impl Default for DeliverySpec {
    fn default() -> Self {
        Self {
            max_chars: DEFAULT_MAX_CHARS,
            output: OutputForm::Plain,
            wake_stream: DeliveryStream::Stderr,
            wake_exit: DEFAULT_WAKE_EXIT,
        }
    }
}

struct Waiter {
    id: u64,
    answer: oneshot::Sender<DaemonMessage>,
}

#[derive(Default)]
struct State {
    parcels: VecDeque<Parcel>,
    waiter: Option<Waiter>,
    next_waiter: u64,
}

struct Shared {
    state: Mutex<State>,
    capacity: usize,
    spec: DeliverySpec,
    status: watch::Receiver<StatusAt>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the daemon may push to the window: not while it cannot tell
    /// what the window is doing, and not once it has ended.
    fn pushable(&self) -> bool {
        !matches!(
            self.status.borrow().status.status(),
            WindowStatus::Unknown | WindowStatus::Exited
        )
    }

    /// Takes the parcels that fit in one answer, oldest first, behind `intro`
    /// when there is one. A parcel is never cut: the first one that does not
    /// fit stays for the next answer.
    fn take_within(&self, state: &mut State, intro: &str) -> (Vec<Parcel>, String) {
        let limit = self.spec.max_chars;
        // An intro longer than the limit cannot be carried whole by any answer.
        let mut text: String = intro.chars().take(limit).collect();
        let mut used = text.chars().count();
        let mut taken = Vec::new();
        while let Some(front) = state.parcels.front() {
            let separator = if text.is_empty() { "" } else { SEPARATOR };
            let needed = separator.chars().count() + front.text.chars().count();
            if used + needed > limit {
                break;
            }
            let Some(parcel) = state.parcels.pop_front() else {
                break;
            };
            text.push_str(separator);
            text.push_str(&parcel.text);
            used += needed;
            taken.push(parcel);
        }
        (taken, text)
    }

    fn read_message(&self, text: &str) -> DaemonMessage {
        DaemonMessage::Delivery {
            output: self.spec.output.render(text),
            stream: DeliveryStream::Stdout,
            exit_code: 0,
        }
    }

    fn wake_message(&self, text: &str) -> DaemonMessage {
        DaemonMessage::Delivery {
            output: self.spec.output.render(text),
            stream: self.spec.wake_stream,
            exit_code: self.spec.wake_exit,
        }
    }

    /// Answers the registered wait with what is waiting, when the window can
    /// be pushed to.
    fn wake(&self, state: &mut State) {
        if state.parcels.is_empty() || !self.pushable() {
            return;
        }
        let Some(waiter) = state.waiter.take() else {
            return;
        };
        let (taken, text) = self.take_within(state, "");
        if waiter.answer.send(self.wake_message(&text)).is_err() {
            // The wait ended between the registration and now: nobody read the
            // answer, so the parcels go back in front of the ones that arrived.
            for parcel in taken.into_iter().rev() {
                state.parcels.push_front(parcel);
            }
        }
    }
}

/// The answer that makes a hook print nothing and succeed.
#[must_use]
pub fn silence() -> DaemonMessage {
    DaemonMessage::Delivery {
        output: String::new(),
        stream: DeliveryStream::Stdout,
        exit_code: 0,
    }
}

/// What registering a wait came to.
pub enum Wait {
    /// A parcel could be pushed at once: this is the answer, and nothing is
    /// registered.
    Answered(Box<DaemonMessage>),
    /// The wait is registered. It ends when `answer` yields, or when `guard`
    /// is dropped (the connection the wait came from is gone).
    Registered {
        /// Removes the registration when dropped.
        guard: WaitGuard,
        /// Yields the answer once a parcel arrives or a newer wait replaces
        /// this one.
        answer: oneshot::Receiver<DaemonMessage>,
    },
}

/// Removes a wait's registration when dropped, unless it was answered or
/// replaced already.
pub struct WaitGuard {
    shared: Arc<Shared>,
    id: u64,
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        if state
            .waiter
            .as_ref()
            .is_some_and(|waiter| waiter.id == self.id)
        {
            state.waiter = None;
        }
    }
}

/// The parcels held for one window, and the one wait that may be registered
/// for it.
#[derive(Clone)]
pub struct Inbox {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Inbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inbox")
            .field("pending", &self.pending_count())
            .field("waiting", &self.has_waiter())
            .finish_non_exhaustive()
    }
}

impl Inbox {
    /// An empty inbox holding at most `capacity` parcels, answering by `spec`
    /// and pushing only while `status` allows it.
    #[must_use]
    pub fn new(capacity: usize, spec: DeliverySpec, status: watch::Receiver<StatusAt>) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                capacity,
                spec,
                status,
            }),
        }
    }

    /// Adds `parcel` and returns the parcels the inbox turned away: the oldest
    /// ones when it is over its capacity, or `parcel` itself when it alone is
    /// longer than one answer can carry. A registered wait is answered when
    /// the window can be pushed to.
    pub fn deposit(&self, parcel: Parcel) -> Vec<Parcel> {
        if parcel.text.chars().count() > self.shared.spec.max_chars {
            return vec![parcel];
        }
        let mut state = self.shared.lock();
        state.parcels.push_back(parcel);
        let mut turned_away = Vec::new();
        while state.parcels.len() > self.shared.capacity {
            let Some(oldest) = state.parcels.pop_front() else {
                break;
            };
            turned_away.push(oldest);
        }
        self.shared.wake(&mut state);
        turned_away
    }

    /// How many parcels wait to be read.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.shared.lock().parcels.len()
    }

    /// Whether a wait is registered.
    #[must_use]
    pub fn has_waiter(&self) -> bool {
        self.shared.lock().waiter.is_some()
    }

    /// The answer for a CLI's start-up: `intro`, then the parcels that fit
    /// behind it. The parcels it carries are taken.
    pub fn context(&self, intro: &str) -> DaemonMessage {
        let mut state = self.shared.lock();
        let (_, text) = self.shared.take_within(&mut state, intro);
        self.shared.read_message(&text)
    }

    /// The answer for the start of a turn: the parcels that fit in one
    /// answer, which are taken.
    pub fn take(&self) -> DaemonMessage {
        self.context("")
    }

    /// Registers a wait for the window, replacing the one registered before:
    /// that one is answered with nothing to print and a success exit.
    pub fn wait(&self) -> Wait {
        let mut state = self.shared.lock();
        if let Some(replaced) = state.waiter.take() {
            // The replaced wait may have ended already; then nobody reads this.
            let _ = replaced.answer.send(silence());
        }
        if !state.parcels.is_empty() && self.shared.pushable() {
            let (_, text) = self.shared.take_within(&mut state, "");
            return Wait::Answered(Box::new(self.shared.wake_message(&text)));
        }
        state.next_waiter += 1;
        let id = state.next_waiter;
        let (answer_tx, answer) = oneshot::channel();
        state.waiter = Some(Waiter {
            id,
            answer: answer_tx,
        });
        Wait::Registered {
            guard: WaitGuard {
                shared: Arc::clone(&self.shared),
                id,
            },
            answer,
        }
    }
}
