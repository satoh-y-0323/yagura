//! A window's inbox: the parcels the daemon holds for the window's CLI to read
//! through the doors its hooks run (`deliver_context`, `deliver_take`,
//! `deliver_wait`).
//!
//! The CLI asks for what is waiting: at start-up, at the start of a turn, or by
//! leaving a wait registered that the daemon answers when a parcel arrives.
//! Anything else the daemon does of its own accord is a push ([`Inbox::begin_push`]):
//! the parcels it carries are out of reach of the doors until it ends, so one
//! parcel never reaches the CLI twice.

use std::collections::VecDeque;
use std::sync::atomic::AtomicUsize;
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
    match value_at_pointer(input, pointer) {
        Some(PointerValue::Text(text)) => Some(text),
        Some(PointerValue::Other) | None => None,
    }
}

/// Whether `input` has a value other than `null` at `pointer`, of any type.
/// Reads the way [`session_at_pointer`] does.
#[must_use]
pub fn has_value_at_pointer(input: &[u8], pointer: &str) -> bool {
    value_at_pointer(input, pointer).is_some()
}

/// What a JSON pointer ends at, short of `null`.
enum PointerValue {
    Text(String),
    Other,
}

fn value_at_pointer(input: &[u8], pointer: &str) -> Option<PointerValue> {
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
    found: &'a mut Option<PointerValue>,
}

impl PointerSeek<'_> {
    /// Records a value that is not a string, when the pointer ends here.
    fn mark_other(self) {
        if self.path.is_empty() {
            *self.found = Some(PointerValue::Other);
        }
    }
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
            *self.found = Some(PointerValue::Text(value.to_string()));
        }
        Ok(())
    }

    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
        self.mark_other();
        Ok(())
    }

    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        self.mark_other();
        Ok(())
    }

    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        self.mark_other();
        Ok(())
    }

    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
        self.mark_other();
        Ok(())
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        use serde::de::{Error, IgnoredAny};
        let Some((token, rest)) = self.path.split_first() else {
            self.mark_other();
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
            self.mark_other();
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
    notice: bool,
}

impl Parcel {
    /// A parcel of `text` named `key`.
    #[must_use]
    pub fn new(key: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            text: text.into(),
            notice: false,
        }
    }

    /// A notice: the daemon's own word, kept once per `key`: depositing one
    /// while another waits under the same `key` replaces it (the replaced
    /// depositor learns nothing more), and its depositor revises or withdraws
    /// it by that key. It is not counted against the inbox's capacity.
    #[must_use]
    pub fn notice(key: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            notice: true,
            ..Self::new(key, text)
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

/// How each door prints the text it carries: the form follows the event the
/// door runs on, not the profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoorOutputs {
    /// The answer for a CLI's start-up.
    pub context: OutputForm,
    /// The answer for the start of a turn.
    pub take: OutputForm,
    /// The answer to a wait.
    pub wait: OutputForm,
}

impl DoorOutputs {
    /// Every door prints in `form`.
    #[must_use]
    pub fn all(form: &OutputForm) -> Self {
        Self {
            context: form.clone(),
            take: form.clone(),
            wait: form.clone(),
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
    pub output: DoorOutputs,
    /// The stream a wait that was answered by a parcel prints on.
    pub wake_stream: DeliveryStream,
    /// The exit code that wait ends with.
    pub wake_exit: u8,
}

impl Default for DeliverySpec {
    fn default() -> Self {
        Self {
            max_chars: DEFAULT_MAX_CHARS,
            output: DoorOutputs::all(&OutputForm::Plain),
            wake_stream: DeliveryStream::Stderr,
            wake_exit: DEFAULT_WAKE_EXIT,
        }
    }
}

/// How a parcel left the inbox, as its depositor learns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParcelEnd {
    /// The window's CLI took it.
    Taken,
    /// It was turned away: the budget a push of it draws on was spent.
    TurnedAway,
    /// The window ended before its CLI took it.
    Closed,
    /// The daemon stopped with the parcel still waiting.
    Stopped,
}

/// A daemon-wide budget a push draws one unit from while it runs.
#[derive(Debug, Clone)]
pub struct PushBudget {
    /// The units in use.
    pub counter: Arc<AtomicUsize>,
    /// The most units there are.
    pub max: usize,
}

/// One parcel as the inbox holds it: who learns how it left, and what its
/// push draws on.
struct Held {
    parcel: Parcel,
    end: Option<oneshot::Sender<ParcelEnd>>,
    budget: Option<PushBudget>,
}

impl Held {
    fn settle(self, end: ParcelEnd) {
        if let Some(tx) = self.end {
            // A depositor that stopped listening has nothing left to learn.
            let _ = tx.send(end);
        }
    }
}

/// Parcels a push has taken out of the inbox and not yet finished with.
struct Carried {
    id: u64,
    held: Vec<Held>,
    /// Raised when the depositor withdraws it while it is under way: the push
    /// gives up if it still can, and if it fails the parcels are dropped
    /// instead of going back.
    withdrawn: watch::Sender<bool>,
    /// Raised once the push has pressed the submit key: what it carries has
    /// reached the CLI, whatever its cleanup still has to do.
    submitted: Arc<watch::Sender<bool>>,
}

struct Waiter {
    id: u64,
    answer: oneshot::Sender<DaemonMessage>,
}

#[derive(Default)]
struct State {
    parcels: VecDeque<Held>,
    carrying: Vec<Carried>,
    next_push: u64,
    /// How the inbox ended, once it has: nothing more is accepted.
    closed: Option<ParcelEnd>,
    waiter: Option<Waiter>,
    next_waiter: u64,
}

impl State {
    /// Removes the waiting parcel named `key`; `false` when none waits.
    fn remove_waiting(&mut self, key: &str) -> bool {
        match self.parcels.iter().position(|held| held.parcel.key == key) {
            Some(position) => {
                self.parcels.remove(position);
                true
            }
            None => false,
        }
    }

    /// How many waiting parcels are not notices: the ones the capacity bounds.
    fn waiting_others(&self) -> usize {
        self.parcels
            .iter()
            .filter(|held| !held.parcel.notice)
            .count()
    }
}

struct Shared {
    state: Mutex<State>,
    capacity: usize,
    spec: DeliverySpec,
    status: watch::Receiver<StatusAt>,
    #[cfg(test)]
    before_withdraw: Mutex<Option<Box<dyn FnOnce() + Send>>>,
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
    fn take_within(&self, state: &mut State, intro: &str) -> (Vec<Held>, String) {
        let limit = self.spec.max_chars;
        // An intro longer than the limit cannot be carried whole by any answer.
        let mut text: String = intro.chars().take(limit).collect();
        let mut used = text.chars().count();
        let mut taken = Vec::new();
        while let Some(front) = state.parcels.front() {
            let separator = if text.is_empty() { "" } else { SEPARATOR };
            let needed = separator.chars().count() + front.parcel.text.chars().count();
            if used + needed > limit {
                break;
            }
            let Some(held) = state.parcels.pop_front() else {
                break;
            };
            text.push_str(separator);
            text.push_str(&held.parcel.text);
            used += needed;
            taken.push(held);
        }
        (taken, text)
    }

    fn read_message(&self, form: &OutputForm, text: &str) -> DaemonMessage {
        DaemonMessage::Delivery {
            output: form.render(text),
            stream: DeliveryStream::Stdout,
            exit_code: 0,
        }
    }

    fn wake_message(&self, text: &str) -> DaemonMessage {
        DaemonMessage::Delivery {
            output: self.spec.output.wait.render(text),
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
            for held in taken.into_iter().rev() {
                state.parcels.push_front(held);
            }
            return;
        }
        for held in taken {
            held.settle(ParcelEnd::Taken);
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

/// What depositing a parcel came to.
pub struct Deposited {
    /// The new parcel itself when it could not be taken in; empty otherwise.
    pub turned_away: Vec<Parcel>,
    /// Yields how the new parcel left the inbox; `None` when it was turned
    /// away at once.
    pub end: Option<oneshot::Receiver<ParcelEnd>>,
}

/// A push under way: what it carries and what it draws on.
pub struct Push {
    /// Names the push to [`Inbox::finish_push`].
    pub id: u64,
    /// The key of the first parcel it carries.
    pub key: String,
    /// The text the push hands over.
    pub text: String,
    /// The budget the first parcel it carries draws on, if any.
    pub budget: Option<PushBudget>,
    /// Raised when what it carries is withdrawn while it is under way.
    pub withdrawn: watch::Receiver<bool>,
    /// To be raised by the push when it presses the submit key.
    pub submitted: Arc<watch::Sender<bool>>,
}

/// How a push ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushEnd {
    /// The CLI has what the push carried.
    Taken,
    /// Nothing reached the CLI: the parcels go back in front of the inbox.
    Failed,
    /// The budget the push draws on was spent: the parcels are turned away.
    TurnedAway,
}

/// What withdrawing a parcel came to.
#[derive(Debug, Clone)]
pub enum Withdrawal {
    /// It was waiting and is out of the inbox: it never reaches the CLI.
    Removed,
    /// A push is carrying it: it ends with that push, and does not come back
    /// if the push fails. `submitted` turns true once the push has pressed
    /// the submit key, which it may do despite the withdrawal: the parcel has
    /// then reached the CLI.
    UnderWay {
        /// Whether the push has pressed the submit key.
        submitted: watch::Receiver<bool>,
    },
    /// It already left the inbox; its end says how.
    Gone,
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
    /// An empty inbox holding at most `capacity` parcels besides notices,
    /// answering by `spec`
    /// and pushing only while `status` allows it.
    #[must_use]
    pub fn new(capacity: usize, spec: DeliverySpec, status: watch::Receiver<StatusAt>) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                capacity,
                spec,
                status,
                #[cfg(test)]
                before_withdraw: Mutex::new(None),
            }),
        }
    }

    /// Adds `parcel` and returns the parcels the inbox turned away: `parcel`
    /// itself when the inbox is full of other parcels, has ended, or `parcel`
    /// alone is longer than one answer can carry. Nothing already inside is
    /// pushed out, except that a notice replaces the notice waiting under its
    /// key, which is not returned (see [`Parcel::notice`]). A registered wait
    /// is answered when the window can be pushed to.
    pub fn deposit(&self, parcel: Parcel) -> Vec<Parcel> {
        self.deposit_tracked(parcel, None, None).turned_away
    }

    /// [`Self::deposit`], telling the depositor how the parcel leaves the
    /// inbox. `budget` is what a push of it draws on. With `waiting_limit`,
    /// a parcel that would find that many already waiting is turned away
    /// sooner than the capacity alone would. A notice that replaces another
    /// leaves the replaced one's end dropped unsettled.
    pub fn deposit_tracked(
        &self,
        parcel: Parcel,
        budget: Option<PushBudget>,
        waiting_limit: Option<usize>,
    ) -> Deposited {
        let refused = |parcel| Deposited {
            turned_away: vec![parcel],
            end: None,
        };
        if parcel.text.chars().count() > self.shared.spec.max_chars {
            return refused(parcel);
        }
        let mut state = self.shared.lock();
        // A notice is one per key, so it bounds itself; it is never turned
        // away, and never counted, for the room other parcels take.
        let full = !parcel.notice
            && (state.waiting_others() >= self.shared.capacity
                || waiting_limit.is_some_and(|limit| state.parcels.len() >= limit));
        if state.closed.is_some() || full {
            return refused(parcel);
        }
        let (tx, rx) = oneshot::channel();
        let held = Held {
            parcel,
            end: Some(tx),
            budget,
        };
        // A notice is one per key: the newer word takes the older one's place
        // in the queue, and the older depositor's end is dropped unsettled.
        let waiting = state.parcels.iter_mut().find(|waiting| {
            held.parcel.notice && waiting.parcel.notice && waiting.parcel.key == held.parcel.key
        });
        match waiting {
            Some(waiting) => {
                tracing::debug!(key = %held.parcel.key, "notice replaced the one waiting under its key");
                *waiting = held;
            }
            None => state.parcels.push_back(held),
        }
        self.shared.wake(&mut state);
        Deposited {
            turned_away: Vec::new(),
            end: Some(rx),
        }
    }

    /// How many parcels wait to be read; the ones a push carries are not
    /// counted.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.shared.lock().parcels.len()
    }

    /// Whether a parcel named `key` is waiting or being pushed.
    #[must_use]
    pub fn holds(&self, key: &str) -> bool {
        let state = self.shared.lock();
        state.parcels.iter().any(|held| held.parcel.key == key)
            || state
                .carrying
                .iter()
                .any(|carried| carried.held.iter().any(|held| held.parcel.key == key))
    }

    /// Runs `hook` at the start of the next [`Self::withdraw`], before it
    /// takes the inbox's lock.
    #[cfg(test)]
    pub(crate) fn before_next_withdraw(&self, hook: Box<dyn FnOnce() + Send>) {
        *self
            .shared
            .before_withdraw
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(hook);
    }

    /// Whether a wait is registered.
    #[must_use]
    pub fn has_waiter(&self) -> bool {
        self.shared.lock().waiter.is_some()
    }

    /// The answer for a CLI's start-up: `intro`, then the parcels that fit
    /// behind it. The parcels it carries are taken.
    pub fn context(&self, intro: &str) -> DaemonMessage {
        self.read(intro, &self.shared.spec.output.context)
    }

    fn read(&self, intro: &str, form: &OutputForm) -> DaemonMessage {
        let mut state = self.shared.lock();
        let (taken, text) = self.shared.take_within(&mut state, intro);
        for held in taken {
            held.settle(ParcelEnd::Taken);
        }
        self.shared.read_message(form, &text)
    }

    /// The answer for the start of a turn: the parcels that fit in one
    /// answer, which are taken.
    pub fn take(&self) -> DaemonMessage {
        self.read("", &self.shared.spec.output.take)
    }

    /// Registers a wait for the window, replacing the one registered before:
    /// that one is answered with nothing to print and a success exit.
    pub fn wait(&self) -> Wait {
        let mut state = self.shared.lock();
        if state.closed.is_some() {
            return Wait::Answered(Box::new(silence()));
        }
        if let Some(replaced) = state.waiter.take() {
            // The replaced wait may have ended already; then nobody reads this.
            let _ = replaced.answer.send(silence());
        }
        if !state.parcels.is_empty() && self.shared.pushable() {
            let (taken, text) = self.shared.take_within(&mut state, "");
            for held in taken {
                held.settle(ParcelEnd::Taken);
            }
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

    /// Answers the registered wait with what is waiting, when the window can
    /// be pushed to now.
    pub fn wake_waiter(&self) {
        let mut state = self.shared.lock();
        self.shared.wake(&mut state);
    }

    /// The budget a push of the oldest waiting parcel would draw on.
    #[must_use]
    pub fn next_budget(&self) -> Option<PushBudget> {
        self.shared
            .lock()
            .parcels
            .front()
            .and_then(|held| held.budget.clone())
    }

    /// Takes what one push hands over out of the inbox: everything that fits
    /// in one answer when `whole`, otherwise the oldest parcel alone. `None`
    /// when nothing is waiting. The doors do not return what a push carries.
    pub fn begin_push(&self, whole: bool) -> Option<Push> {
        let mut state = self.shared.lock();
        let held = if whole {
            self.shared.take_within(&mut state, "").0
        } else {
            state.parcels.pop_front().into_iter().collect()
        };
        let first = held.first()?;
        let text = held
            .iter()
            .map(|held| held.parcel.text.as_str())
            .collect::<Vec<_>>()
            .join(SEPARATOR);
        let budget = first.budget.clone();
        let key = first.parcel.key.clone();
        state.next_push += 1;
        let id = state.next_push;
        let (withdrawn_tx, withdrawn) = watch::channel(false);
        let submitted = Arc::new(watch::channel(false).0);
        state.carrying.push(Carried {
            id,
            held,
            withdrawn: withdrawn_tx,
            submitted: Arc::clone(&submitted),
        });
        Some(Push {
            id,
            key,
            text,
            budget,
            withdrawn,
            submitted,
        })
    }

    /// Whether what push `id` carries was withdrawn while it was under way.
    #[must_use]
    pub fn push_withdrawn(&self, id: u64) -> bool {
        self.shared
            .lock()
            .carrying
            .iter()
            .any(|carried| carried.id == id && *carried.withdrawn.borrow())
    }

    /// Ends push `id`: what it carried is taken, goes back in front of the
    /// inbox, or is turned away.
    pub fn finish_push(&self, id: u64, end: PushEnd) {
        let mut state = self.shared.lock();
        let Some(position) = state.carrying.iter().position(|carried| carried.id == id) else {
            return;
        };
        let carried = state.carrying.remove(position);
        match end {
            PushEnd::Taken => {
                for held in carried.held {
                    held.settle(ParcelEnd::Taken);
                }
            }
            PushEnd::TurnedAway => {
                for held in carried.held {
                    held.settle(ParcelEnd::TurnedAway);
                }
            }
            // A withdrawn parcel is dropped unsettled: its depositor answers
            // for it.
            PushEnd::Failed if *carried.withdrawn.borrow() => {}
            PushEnd::Failed => match state.closed {
                Some(closed) => {
                    for held in carried.held {
                        held.settle(closed);
                    }
                }
                None => {
                    for held in carried.held.into_iter().rev() {
                        state.parcels.push_front(held);
                    }
                    self.shared.wake(&mut state);
                }
            },
        }
    }

    /// Withdraws the parcel named `key`. A waiting one is removed under the
    /// same lock the doors take it under, so once this returns
    /// [`Withdrawal::Removed`] nothing hands it to the CLI; its depositor
    /// learns nothing more from it.
    pub fn withdraw(&self, key: &str) -> Withdrawal {
        #[cfg(test)]
        {
            let hook = self
                .shared
                .before_withdraw
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(hook) = hook {
                hook();
            }
        }
        let mut state = self.shared.lock();
        if state.remove_waiting(key) {
            return Withdrawal::Removed;
        }
        match state
            .carrying
            .iter_mut()
            .find(|carried| carried.held.iter().any(|held| held.parcel.key == key))
        {
            Some(carried) => {
                carried.withdrawn.send_replace(true);
                Withdrawal::UnderWay {
                    submitted: carried.submitted.subscribe(),
                }
            }
            None => Withdrawal::Gone,
        }
    }

    /// Removes the parcel named `key` if it is still waiting, and reports
    /// whether it did. A parcel a push is carrying is left alone, and so is
    /// everything else that push carries: unlike [`Self::withdraw`], this
    /// never marks a push withdrawn.
    pub fn withdraw_waiting(&self, key: &str) -> bool {
        self.shared.lock().remove_waiting(key)
    }

    /// Replaces the text of the waiting parcel named `key`. `false` when no
    /// such parcel waits, or the new text is longer than one answer carries.
    pub fn revise(&self, key: &str, text: String) -> bool {
        if text.chars().count() > self.shared.spec.max_chars {
            return false;
        }
        let mut state = self.shared.lock();
        match state.parcels.iter_mut().find(|held| held.parcel.key == key) {
            Some(held) => {
                held.parcel.text = text;
                true
            }
            None => false,
        }
    }

    /// Ends the inbox: every waiting parcel ends as `end`, a push under way
    /// ends that way if it fails, the registered wait is answered with
    /// nothing, and nothing more is accepted.
    pub fn close(&self, end: ParcelEnd) {
        let mut state = self.shared.lock();
        if state.closed.is_some() {
            return;
        }
        state.closed = Some(end);
        for held in std::mem::take(&mut state.parcels) {
            held.settle(end);
        }
        if let Some(waiter) = state.waiter.take() {
            let _ = waiter.answer.send(silence());
        }
    }
}
