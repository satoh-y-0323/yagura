//! A window's inbox: the parcels the daemon holds for the window's CLI to read
//! through the doors its hooks run (`deliver_context`, `deliver_take`,
//! `deliver_wait`).
//!
//! The CLI asks for what is waiting: at start-up, at the start of a turn, or by
//! leaving a wait registered that the daemon answers when a parcel arrives.
//! Anything else the daemon does of its own accord is a push ([`Inbox::begin_push`]):
//! the parcels it carries are out of reach of the doors until it ends, so one
//! parcel never reaches the CLI twice.
//!
//! A door's read takes its parcels into a [`DoorHold`]: they stay the
//! daemon's, out of reach of every other door and push, until the door says it
//! printed them ([`DoorHold::confirm`]), which commits their end. A hold
//! dropped before that puts them back. Withdrawing a parcel takes only one
//! that still waits; one a door or a push holds ends with that door or push.
//!
//! An inbox given an [`InboxKeeper`] is the copy of what the store keeps for
//! its window: each change is sent to the store under the inbox's lock. A
//! parcel it keeps is shown to no door, push or wait until its row is
//! written, and the parcels show in the order they were deposited: one whose
//! row could not be written never shows, and holds nothing back.

use std::collections::VecDeque;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use tokio::sync::{oneshot, watch};
use yagura_protocol::{DaemonMessage, DeliveryStream, WindowStatus};

use crate::parcel_store::{ChatDelivery, InboxKeeper, KeptParcel, RowEnd, Written};
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

/// What a JSON pointer ends at, short of `null`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointerValue {
    /// A string.
    Text(String),
    /// A number, a boolean, an object or an array.
    Other,
}

impl PointerValue {
    /// The string, when the value is one.
    #[must_use]
    pub fn into_text(self) -> Option<String> {
        match self {
            Self::Text(text) => Some(text),
            Self::Other => None,
        }
    }
}

/// The values at `pointers` (JSON pointers, RFC 6901) in the JSON document
/// `input` yields, a hook's stdin, one per pointer in the same order; `None`
/// where nothing other than `null` is at the pointer, the pointer does not
/// start with `/`, or `input` is not JSON up to the value.
///
/// The document is read once, as a stream and never held whole, only as far
/// as the last value found (the end of the document when one is missing), so
/// a value is found wherever it sits and `input` is left right after what was
/// read. A document that is cut off is read up to the cut. Where a key appears
/// twice in an object, the first one is read.
#[must_use]
pub fn values_at_pointers(
    input: impl std::io::Read,
    pointers: &[&str],
) -> Vec<Option<PointerValue>> {
    let paths: Vec<Option<Vec<String>>> = pointers
        .iter()
        .map(|pointer| {
            let tokens = pointer.strip_prefix('/')?.split('/');
            Some(
                tokens
                    .map(|token| token.replace("~1", "/").replace("~0", "~"))
                    .collect(),
            )
        })
        .collect();
    let mut found = vec![None; pointers.len()];
    let targets: Vec<Target<'_>> = paths
        .iter()
        .enumerate()
        .filter_map(|(slot, path)| {
            Some(Target {
                slot,
                path: path.as_deref()?,
            })
        })
        .collect();
    if targets.is_empty() {
        return found;
    }
    let mut deserializer = serde_json::Deserializer::from_reader(input);
    // Reading stops once every value is found, at the end of the document, or
    // fails at the cut; either way the answer is what `found` holds.
    let _ = serde::de::DeserializeSeed::deserialize(
        PointerSeek {
            targets,
            found: &mut found,
        },
        &mut deserializer,
    );
    found
}

/// A pointer's remaining tokens below the value being read, and where its
/// value is recorded.
struct Target<'a> {
    slot: usize,
    path: &'a [String],
}

/// Walks a JSON document along several pointers at once, recording the values
/// they end at.
struct PointerSeek<'s, 'a> {
    targets: Vec<Target<'a>>,
    found: &'s mut [Option<PointerValue>],
}

impl<'a> PointerSeek<'_, 'a> {
    /// Records `value` for every pointer that ends here; returns the pointers
    /// that go further.
    fn record_here(&mut self, value: &PointerValue) -> Vec<Target<'a>> {
        let mut deeper = Vec::new();
        for target in &self.targets {
            if target.path.is_empty() {
                self.found[target.slot] = Some(value.clone());
            } else {
                deeper.push(Target {
                    slot: target.slot,
                    path: target.path,
                });
            }
        }
        deeper
    }

    fn record_scalar(mut self, value: &PointerValue) {
        let _ = self.record_here(value);
    }

    fn all_found(&self) -> bool {
        self.found.iter().all(Option::is_some)
    }
}

impl<'de> serde::de::DeserializeSeed<'de> for PointerSeek<'_, '_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for PointerSeek<'_, '_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON document")
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<(), E> {
        self.record_scalar(&PointerValue::Text(value.to_string()));
        Ok(())
    }

    // `null` is no value; reading goes on for the other pointers.
    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
        self.record_scalar(&PointerValue::Other);
        Ok(())
    }

    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        self.record_scalar(&PointerValue::Other);
        Ok(())
    }

    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        self.record_scalar(&PointerValue::Other);
        Ok(())
    }

    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
        self.record_scalar(&PointerValue::Other);
        Ok(())
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(mut self, mut map: A) -> Result<(), A::Error> {
        use serde::de::{Error, IgnoredAny};
        let mut open: Vec<(usize, &str, &[String])> = self
            .record_here(&PointerValue::Other)
            .into_iter()
            .filter_map(|target| {
                let (token, rest) = target.path.split_first()?;
                Some((target.slot, token.as_str(), rest))
            })
            .collect();
        while let Some(key) = map.next_key::<String>()? {
            let (matched, rest): (Vec<_>, Vec<_>) =
                open.into_iter().partition(|(_, token, _)| *token == key);
            open = rest;
            if matched.is_empty() {
                map.next_value::<IgnoredAny>()?;
                continue;
            }
            map.next_value_seed(PointerSeek {
                targets: matched
                    .into_iter()
                    .map(|(slot, _, path)| Target { slot, path })
                    .collect(),
                found: &mut *self.found,
            })?;
            if self.all_found() {
                return Err(A::Error::custom("every pointer reached"));
            }
        }
        Ok(())
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(mut self, mut seq: A) -> Result<(), A::Error> {
        use serde::de::{Error, IgnoredAny};
        let mut open: Vec<(usize, usize, &[String])> = self
            .record_here(&PointerValue::Other)
            .into_iter()
            .filter_map(|target| {
                let (token, rest) = target.path.split_first()?;
                Some((target.slot, token.parse::<usize>().ok()?, rest))
            })
            .collect();
        for position in 0.. {
            let (matched, rest): (Vec<_>, Vec<_>) = open
                .into_iter()
                .partition(|(_, index, _)| *index == position);
            open = rest;
            let element = if matched.is_empty() {
                seq.next_element::<IgnoredAny>()?.map(drop)
            } else {
                seq.next_element_seed(PointerSeek {
                    targets: matched
                        .into_iter()
                        .map(|(slot, _, path)| Target { slot, path })
                        .collect(),
                    found: &mut *self.found,
                })?
            };
            if element.is_none() {
                break;
            }
            if self.all_found() {
                return Err(A::Error::custom("every pointer reached"));
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
    saved: bool,
}

impl Parcel {
    /// A parcel of `text` named `key`, kept in the store (when the inbox has
    /// one) so a next daemon hands it out.
    #[must_use]
    pub fn new(key: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            text: text.into(),
            notice: false,
            saved: true,
        }
    }

    /// This parcel held in memory alone: it ends with the daemon, and no
    /// next daemon hands it out. A kind of parcel moved to unsaved needs a
    /// migration in the same change that deletes the rows older daemons kept
    /// for it, or a next daemon still hands those out.
    #[must_use]
    pub fn unsaved(self) -> Self {
        Self {
            saved: false,
            ..self
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
    /// A push handed it to the window's CLI, and its row's removal recorded
    /// the chat delivery it carried as taken: its depositor writes nothing
    /// more.
    TakenRecorded,
    /// It was turned away: the budget a push of it draws on was spent.
    TurnedAway,
    /// The window ended before its CLI took it.
    Closed,
    /// The daemon stopped with the parcel still waiting.
    Stopped,
}

/// Where a deposited parcel is, as its depositor watches it until it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Waiting in the inbox.
    Waiting,
    /// A door or a push is carrying it: it ends with them, or comes back to
    /// waiting when they fail.
    Carried,
}

/// A daemon-wide budget a push draws one unit from while it runs.
#[derive(Debug, Clone)]
pub struct PushBudget {
    /// The units in use.
    pub counter: Arc<AtomicUsize>,
    /// The most units there are.
    pub max: usize,
}

/// Who writes a deposited parcel's row.
enum Keeping {
    /// The inbox's own keeper.
    Keeper,
    /// The depositor, in a transaction of its own with the chat delivery the
    /// parcel carries.
    Depositor(ChatDelivery),
}

/// How a deposited parcel's row came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveOutcome {
    /// Its row is written, or it needs none: it is unsaved, the inbox keeps
    /// nothing, or it was turned away.
    Saved,
    /// Its window has no row to keep it with.
    NoRow,
    /// The write failed.
    Failed,
}

impl SaveOutcome {
    pub(crate) fn of(written: Written) -> Self {
        match written {
            Written::Rows(0) => Self::NoRow,
            Written::Rows(_) => Self::Saved,
            Written::Failed => Self::Failed,
        }
    }
}

/// Yields a deposit's [`SaveOutcome`] once its write has told it.
pub(crate) struct Saving(Option<oneshot::Receiver<SaveOutcome>>);

impl Saving {
    /// A deposit that needed no write.
    pub(crate) fn ready() -> Self {
        Self(None)
    }

    /// A deposit whose outcome `outcome` yields; one never told failed.
    pub(crate) fn of(outcome: oneshot::Receiver<SaveOutcome>) -> Self {
        Self(Some(outcome))
    }

    pub(crate) async fn outcome(self) -> SaveOutcome {
        match self.0 {
            None => SaveOutcome::Saved,
            Some(outcome) => outcome.await.unwrap_or(SaveOutcome::Failed),
        }
    }
}

/// A parcel deposited by a depositor that writes its row itself, in a
/// transaction of its own: held back from every door until [`Self::settle`]
/// tells how that went. Dropped unsettled, it counts as a write that failed.
pub(crate) struct Unwritten {
    shared: Weak<Shared>,
    id: u64,
    settled: bool,
}

impl Unwritten {
    /// Tells how the row's write came out: the parcel shows, or, its row not
    /// written, leaves without ever showing.
    pub(crate) fn settle(mut self, outcome: SaveOutcome) {
        self.settled = true;
        if let Some(shared) = self.shared.upgrade() {
            shared.settle_write(self.id, outcome);
        }
    }
}

impl Drop for Unwritten {
    fn drop(&mut self) {
        if !self.settled
            && let Some(shared) = self.shared.upgrade()
        {
            shared.settle_write(self.id, SaveOutcome::Failed);
        }
    }
}

/// One parcel as the inbox holds it: who learns how it left, and what its
/// push draws on.
struct Held {
    parcel: Parcel,
    /// Its place in the inbox as the store keeps it: lower is older.
    seq: i64,
    /// Names it to the write that tells how its row came out.
    id: u64,
    /// Whether its row is written (or it needs none): only then may it show.
    written: bool,
    /// Whether its depositor woke the window's agent for it, which a parcel
    /// held back has to be woken for again once it shows.
    wakes: bool,
    /// Whether its depositor forgets its row itself, in the same transaction
    /// that settles what the parcel was for, once it is taken or turned
    /// away: the inbox then leaves that row alone.
    depositor_forgets: bool,
    /// The chat delivery its row's removal records, for a depositor that
    /// forgets the row itself.
    delivery: Option<ChatDelivery>,
    /// Whether a push's settling has forgotten its row already (or owes
    /// that): nothing forgets it a second time.
    settled: bool,
    end: Option<oneshot::Sender<ParcelEnd>>,
    budget: Option<PushBudget>,
    /// Where it is, for its depositor to watch.
    place: watch::Sender<Place>,
}

impl Held {
    fn move_to(&self, place: Place) {
        self.place.send_replace(place);
    }

    fn kept(&self) -> KeptParcel {
        KeptParcel {
            seq: self.seq,
            key: self.parcel.key.clone(),
            notice: self.parcel.notice,
            text: self.parcel.text.clone(),
        }
    }

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
    /// Raised once the push has pressed the submit key: what it carries has
    /// reached the CLI, whatever its cleanup still has to do.
    submitted: Arc<watch::Sender<bool>>,
    /// The settling before the submit key, once it was asked for.
    settling: Option<Arc<Mutex<Recorded>>>,
}

/// Whether a push's settling has committed: until it has, the ends of the
/// chat deliveries it records wait for it.
enum Recorded {
    Waiting(Vec<oneshot::Sender<ParcelEnd>>),
    Committed,
}

impl Recorded {
    /// Tells `end` that its delivery is recorded, once the settling has
    /// committed.
    fn tell(recorded: &Mutex<Self>, end: oneshot::Sender<ParcelEnd>) {
        match &mut *recorded.lock().unwrap_or_else(PoisonError::into_inner) {
            Self::Waiting(ends) => ends.push(end),
            // A depositor that stopped listening has nothing left to learn.
            Self::Committed => drop(end.send(ParcelEnd::TakenRecorded)),
        }
    }

    fn commit(recorded: &Mutex<Self>) {
        let ends = std::mem::replace(
            &mut *recorded.lock().unwrap_or_else(PoisonError::into_inner),
            Self::Committed,
        );
        if let Self::Waiting(ends) = ends {
            for end in ends {
                let _ = end.send(ParcelEnd::TakenRecorded);
            }
        }
    }
}

struct Waiter {
    id: u64,
    answer: oneshot::Sender<DoorRead>,
}

/// Parcels a door's read has taken out of the inbox and its door has not
/// said it printed yet.
struct DoorHeld {
    id: u64,
    held: Vec<Held>,
    /// Never raised: it is dropped when the door ends the hold, which is what
    /// a withdrawal that found the parcels here waits for.
    ended: watch::Sender<bool>,
}

#[derive(Default)]
struct State {
    parcels: VecDeque<Held>,
    carrying: Vec<Carried>,
    doors: Vec<DoorHeld>,
    /// Names pushes and door holds alike, to the writer that settles them.
    next_push: u64,
    /// How the inbox ended, once it has: nothing more is accepted.
    closed: Option<ParcelEnd>,
    waiter: Option<Waiter>,
    next_waiter: u64,
    /// The place the next parcel deposited takes.
    next_seq: i64,
    next_id: u64,
}

impl State {
    /// How many parcels show: the ones ahead of the first whose row is not
    /// written yet.
    fn shown(&self) -> usize {
        self.parcels.iter().take_while(|held| held.written).count()
    }

    fn take_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Removes the waiting parcel named `key`; `None` when no parcel waiting
    /// has it.
    fn remove_waiting(&mut self, key: &str) -> Option<Held> {
        let position = self
            .parcels
            .iter()
            .position(|held| held.parcel.key == key)?;
        self.parcels.remove(position)
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
    /// Itself, for the holds it hands out.
    me: Weak<Shared>,
    state: Mutex<State>,
    capacity: usize,
    spec: DeliverySpec,
    status: watch::Receiver<StatusAt>,
    keeper: Option<InboxKeeper>,
    #[cfg(test)]
    before_withdraw: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps `held`, unless it is unsaved, holding it back from showing
    /// until its row is written. Called under the inbox's lock, so the store
    /// sees the changes in the order the inbox made them.
    fn keep(self: &Arc<Self>, held: &mut Held) -> Saving {
        let Some(keeper) = self.keeper.as_ref().filter(|_| held.parcel.saved) else {
            return Saving::ready();
        };
        held.written = false;
        let (told, outcome) = oneshot::channel();
        let shared = Arc::downgrade(self);
        let id = held.id;
        keeper.keep(
            held.kept(),
            Box::new(move |written| {
                let outcome = SaveOutcome::of(written);
                let wake =
                    Weak::upgrade(&shared).is_some_and(|shared| shared.settle_write(id, outcome));
                // A depositor that stopped listening has nothing left to learn.
                let _ = told.send(outcome);
                wake
            }),
        );
        Saving::of(outcome)
    }

    /// [`Self::keep`], or, for a depositor that writes the row itself, holds
    /// `held` back and hands the row to write to `row`.
    fn keep_by(
        self: &Arc<Self>,
        held: &mut Held,
        keeping: Keeping,
        row: &mut Option<(KeptParcel, Unwritten)>,
    ) -> Saving {
        match keeping {
            Keeping::Keeper => self.keep(held),
            Keeping::Depositor(delivery) => {
                if self.keeper.is_some() && held.parcel.saved {
                    held.written = false;
                    held.depositor_forgets = true;
                    held.delivery = Some(delivery);
                    *row = Some((
                        held.kept(),
                        Unwritten {
                            shared: Arc::downgrade(self),
                            id: held.id,
                            settled: false,
                        },
                    ));
                }
                Saving::ready()
            }
        }
    }

    /// Settles the write of parcel `id`: it may show now, or, its row not
    /// written, it leaves without ever showing. Returns whether that showed a
    /// parcel whose depositor woke the window's agent for it.
    fn settle_write(&self, id: u64, outcome: SaveOutcome) -> bool {
        let mut state = self.lock();
        let before = state.shown();
        // A parcel that left before its row was written has nothing to settle.
        let Some(position) = state.parcels.iter().position(|held| held.id == id) else {
            return false;
        };
        if outcome == SaveOutcome::Saved {
            if let Some(held) = state.parcels.get_mut(position) {
                held.written = true;
            }
        } else {
            // Dropped unsettled: its depositor learns from the write instead.
            state.parcels.remove(position);
        }
        let after = state.shown();
        if after == before {
            return false;
        }
        self.wake(&mut state);
        state
            .parcels
            .iter()
            .take(after)
            .skip(before)
            .any(|held| held.wakes)
    }

    /// Forgets `held`, which left the inbox for good. Called under the
    /// inbox's lock.
    fn forget<'a>(&self, held: impl IntoIterator<Item = &'a Held>) {
        if let Some(keeper) = &self.keeper {
            keeper.forget(
                held.into_iter()
                    .filter(|held| held.parcel.saved)
                    .map(|held| held.seq)
                    .collect(),
            );
        }
    }

    /// [`Self::forget`] for parcels that were taken or turned away: a parcel
    /// whose depositor forgets its row itself keeps it until then.
    fn forget_ended<'a>(&self, held: impl IntoIterator<Item = &'a Held>) {
        self.forget(
            held.into_iter()
                .filter(|held| !held.depositor_forgets && !held.settled),
        );
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
        self.take_within_limit(state, intro, self.spec.max_chars)
    }

    /// [`Shared::take_within`] with the answer's limit given by the caller.
    fn take_within_limit(
        &self,
        state: &mut State,
        intro: &str,
        limit: usize,
    ) -> (Vec<Held>, String) {
        // An intro longer than the limit cannot be carried whole by any answer.
        let mut text: String = intro.chars().take(limit).collect();
        let mut used = text.chars().count();
        let mut taken = Vec::new();
        while let Some(front) = state.parcels.front().filter(|held| held.written) {
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
            hook_timeout_sec: None,
        }
    }

    fn wake_message(&self, text: &str) -> DaemonMessage {
        DaemonMessage::Delivery {
            output: self.spec.output.wait.render(text),
            stream: self.spec.wake_stream,
            exit_code: self.spec.wake_exit,
            hook_timeout_sec: None,
        }
    }

    /// Answers the registered wait with what is waiting, when the window can
    /// be pushed to.
    fn wake(&self, state: &mut State) {
        if state.shown() == 0 || !self.pushable() {
            return;
        }
        let Some(waiter) = state.waiter.take() else {
            return;
        };
        let (taken, text) = self.take_within(state, "");
        let read = DoorRead {
            message: self.wake_message(&text),
            hold: self.hold(state, taken),
        };
        if let Err(mut read) = waiter.answer.send(read) {
            // The wait ended between the registration and now: nobody read the
            // answer, so the parcels go back in front of the ones that arrived.
            if let Some(id) = read.hold.id.take()
                && let Some(door) = state.remove_door(id)
            {
                self.put_back(state, door.held);
            }
        }
    }

    /// Holds `taken` for a door until it confirms them.
    fn hold(&self, state: &mut State, taken: Vec<Held>) -> DoorHold {
        if taken.is_empty() {
            return DoorHold::empty();
        }
        for held in &taken {
            held.move_to(Place::Carried);
        }
        state.next_push += 1;
        let id = state.next_push;
        state.doors.push(DoorHeld {
            id,
            held: taken,
            ended: watch::channel(false).0,
        });
        DoorHold {
            shared: self.me.clone(),
            id: Some(id),
        }
    }

    /// Puts `held`, which no door took after all, back where its place in
    /// the inbox is; or ends it with the inbox, once that has ended.
    fn put_back(&self, state: &mut State, held: Vec<Held>) {
        if let Some(closed) = state.closed {
            for held in held {
                held.settle(closed);
            }
            return;
        }
        for held in held {
            held.move_to(Place::Waiting);
            let position = state
                .parcels
                .iter()
                .position(|waiting| waiting.seq > held.seq)
                .unwrap_or(state.parcels.len());
            state.parcels.insert(position, held);
        }
        self.wake(state);
    }

    /// Commits the end of what door hold `id` took, once its door printed it:
    /// the rows of the kept parcels are forgotten, with the chat delivery each
    /// carries recorded as taken, in one transaction; then each depositor
    /// learns it was taken.
    async fn confirm(self: Arc<Self>, id: u64) {
        let (rows, recorded) = {
            let mut state = self.lock();
            let Some(door) = state.doors.iter_mut().find(|door| door.id == id) else {
                return;
            };
            let mut rows = Vec::new();
            if self.keeper.is_some() {
                for held in door.held.iter_mut().filter(|held| held.parcel.saved) {
                    if held.delivery.is_none() && held.depositor_forgets {
                        continue;
                    }
                    rows.push(RowEnd {
                        parcel: held.kept(),
                        delivery: held.delivery.clone(),
                    });
                    held.settled = true;
                }
            }
            (rows, Arc::new(Mutex::new(Recorded::Waiting(Vec::new()))))
        };
        if let Some(keeper) = self.keeper.as_ref().filter(|_| !rows.is_empty()) {
            let committed = Arc::clone(&recorded);
            let settled = keeper.settle_push(
                id,
                rows,
                None,
                Box::new(move || Recorded::commit(&committed)),
            );
            let _ = settled.await;
        }
        let mut state = self.lock();
        if let Some(door) = state.remove_door(id) {
            for mut held in door.held {
                match (held.settled && held.delivery.is_some(), held.end.take()) {
                    (true, Some(end)) => Recorded::tell(&recorded, end),
                    (_, end) => {
                        held.end = end;
                        held.settle(ParcelEnd::Taken);
                    }
                }
            }
        }
    }

    /// Whether door hold `id` took a parcel whose row the store keeps.
    fn door_keeps_rows(&self, id: u64) -> bool {
        self.keeper.is_some()
            && self
                .lock()
                .doors
                .iter()
                .any(|door| door.id == id && door.held.iter().any(|held| held.parcel.saved))
    }

    /// Puts back what door hold `id` took: its door went away before it
    /// printed.
    fn release_door(&self, id: u64) {
        let mut state = self.lock();
        if let Some(door) = state.remove_door(id) {
            self.put_back(&mut state, door.held);
        }
    }
}

impl State {
    fn remove_door(&mut self, id: u64) -> Option<DoorHeld> {
        let position = self.doors.iter().position(|door| door.id == id)?;
        Some(self.doors.remove(position))
    }
}

/// What a door is answered with, and the parcels that answer took.
pub struct DoorRead {
    /// The answer.
    pub message: DaemonMessage,
    /// The parcels it carries, held until the door confirms them.
    pub hold: DoorHold,
}

impl DoorRead {
    /// The answer that makes a hook print nothing and succeed, taking nothing.
    #[must_use]
    pub fn silent() -> Self {
        Self {
            message: silence(),
            hold: DoorHold::empty(),
        }
    }
}

/// The parcels a door's answer took, out of reach of every other door and
/// push until [`Self::confirm`] ends them; dropped before that, they go back.
pub struct DoorHold {
    shared: Weak<Shared>,
    /// `None` once ended, or when the answer took nothing.
    id: Option<u64>,
}

impl std::fmt::Debug for DoorHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DoorHold")
            .field("holds", &self.id.is_some())
            .finish_non_exhaustive()
    }
}

impl DoorHold {
    fn empty() -> Self {
        Self {
            shared: Weak::new(),
            id: None,
        }
    }

    /// Whether it holds a parcel whose row the store keeps.
    #[must_use]
    pub fn keeps_rows(&self) -> bool {
        match (self.id, self.shared.upgrade()) {
            (Some(id), Some(shared)) => shared.door_keeps_rows(id),
            _ => false,
        }
    }

    /// Ends the read once its door printed what it took: when the end of what
    /// it took is committed, its depositors learn it was taken. A future
    /// dropped early still finishes the confirmation.
    pub async fn confirm(mut self) {
        let (Some(id), Some(shared)) = (self.id.take(), self.shared.upgrade()) else {
            return;
        };
        // A confirmation that panicked left nothing to tell: the hold is gone.
        let _ = tokio::spawn(shared.confirm(id)).await;
    }
}

impl Drop for DoorHold {
    fn drop(&mut self) {
        if let (Some(id), Some(shared)) = (self.id.take(), self.shared.upgrade()) {
            shared.release_door(id);
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
        hook_timeout_sec: None,
    }
}

/// What registering a wait came to.
pub enum Wait {
    /// A parcel could be pushed at once: this is the answer, and nothing is
    /// registered.
    Answered(Box<DoorRead>),
    /// The wait is registered. It ends when `answer` yields, or when `guard`
    /// is dropped (the connection the wait came from is gone).
    Registered {
        /// Removes the registration when dropped.
        guard: WaitGuard,
        /// Yields the answer once a parcel arrives or a newer wait replaces
        /// this one.
        answer: oneshot::Receiver<DoorRead>,
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
    /// Whether the deposit itself answered the registered wait with the new
    /// parcel, under the same lock it was put in under.
    pub handed_to_wait: bool,
    /// Where the new parcel is until it ends (the sender closes then);
    /// `None` when it was turned away at once.
    pub place: Option<watch::Receiver<Place>>,
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
    /// A door or a push holds it: it ends with that door or push, and goes
    /// back to waiting when they fail. `submitted` turns true once a push has
    /// pressed the submit key: the parcel has then reached the CLI. A door's
    /// is never raised. Either closes when the door or push ends, which is when
    /// the parcel is waiting again or ended.
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
        Self::kept(capacity, spec, status, None, Vec::new())
    }

    /// [`Self::new`], keeping every change through `keeper` and holding
    /// `kept` first: the parcels the store kept for the window, oldest first,
    /// whose depositors are gone. A kept parcel longer than one answer
    /// carries is dropped, as a deposit of it would have been turned away.
    pub(crate) fn kept(
        capacity: usize,
        spec: DeliverySpec,
        status: watch::Receiver<StatusAt>,
        keeper: Option<InboxKeeper>,
        kept: Vec<KeptParcel>,
    ) -> Self {
        let mut state = State::default();
        let mut too_long = Vec::new();
        for parcel in kept {
            state.next_seq = state.next_seq.max(parcel.seq.saturating_add(1));
            if parcel.text.chars().count() > spec.max_chars {
                too_long.push(parcel.seq);
                continue;
            }
            let id = state.take_id();
            state.parcels.push_back(Held {
                parcel: Parcel {
                    key: parcel.key,
                    text: parcel.text,
                    notice: parcel.notice,
                    saved: true,
                },
                seq: parcel.seq,
                id,
                written: true,
                wakes: false,
                depositor_forgets: false,
                delivery: None,
                settled: false,
                end: None,
                budget: None,
                place: watch::channel(Place::Waiting).0,
            });
        }
        if let Some(keeper) = keeper.as_ref().filter(|_| !too_long.is_empty()) {
            tracing::warn!(
                count = too_long.len(),
                "dropped kept parcels longer than the window's answers carry"
            );
            keeper.forget(too_long);
        }
        Self {
            shared: Arc::new_cyclic(|me| Shared {
                me: me.clone(),
                state: Mutex::new(state),
                capacity,
                spec,
                status,
                keeper,
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
        self.deposit_as(parcel, None, None, false).0.turned_away
    }

    /// [`Self::deposit`], telling the depositor how the parcel leaves the
    /// inbox. `budget` is what a push of it draws on. With `waiting_limit`,
    /// a parcel that would find that many already waiting is turned away
    /// sooner than the capacity alone would. A notice that replaces another
    /// leaves the replaced one's end dropped unsettled. A kept parcel shows
    /// once its row is written, and one whose row cannot be written leaves
    /// without ever showing. The depositor wakes the window's agent: one
    /// held back is woken for again once it shows.
    pub fn deposit_tracked(
        &self,
        parcel: Parcel,
        budget: Option<PushBudget>,
        waiting_limit: Option<usize>,
    ) -> Deposited {
        self.deposit_as(parcel, budget, waiting_limit, true).0
    }

    /// [`Self::deposit_tracked`], with [`Saving`] telling the depositor how
    /// the parcel's row came out, for a depositor that answers for it being
    /// kept.
    pub(crate) fn deposit_saved(
        &self,
        parcel: Parcel,
        budget: Option<PushBudget>,
        waiting_limit: Option<usize>,
    ) -> (Deposited, Saving) {
        self.deposit_as(parcel, budget, waiting_limit, true)
    }

    /// [`Self::deposit_tracked`] for a depositor that writes the parcel's row
    /// itself, in one transaction with `delivery`, the chat delivery the
    /// parcel carries. The row to write comes back with the [`Unwritten`]
    /// that holds the parcel back until the depositor tells how the write
    /// came out; `None` when the inbox keeps nothing. The parcel's row is
    /// forgotten by its depositor too, once it is taken or turned away, but
    /// for a push's settling, which records `delivery` with it
    /// ([`ParcelEnd::TakenRecorded`]).
    pub(crate) fn deposit_with_row(
        &self,
        parcel: Parcel,
        budget: Option<PushBudget>,
        delivery: ChatDelivery,
    ) -> (Deposited, Option<(KeptParcel, Unwritten)>) {
        let (deposited, _, row) =
            self.deposit_by(parcel, budget, None, true, Keeping::Depositor(delivery));
        (deposited, row)
    }

    fn deposit_as(
        &self,
        parcel: Parcel,
        budget: Option<PushBudget>,
        waiting_limit: Option<usize>,
        wakes: bool,
    ) -> (Deposited, Saving) {
        let (deposited, saving, _) =
            self.deposit_by(parcel, budget, waiting_limit, wakes, Keeping::Keeper);
        (deposited, saving)
    }

    fn deposit_by(
        &self,
        parcel: Parcel,
        budget: Option<PushBudget>,
        waiting_limit: Option<usize>,
        wakes: bool,
        keeping: Keeping,
    ) -> (Deposited, Saving, Option<(KeptParcel, Unwritten)>) {
        let refused = |parcel| {
            (
                Deposited {
                    turned_away: vec![parcel],
                    end: None,
                    handed_to_wait: false,
                    place: None,
                },
                Saving::ready(),
                None,
            )
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
        let id = state.take_id();
        let mut held = Held {
            parcel,
            seq: state.next_seq,
            id,
            written: true,
            wakes,
            depositor_forgets: false,
            delivery: None,
            settled: false,
            end: Some(tx),
            budget,
            place: watch::channel(Place::Waiting).0,
        };
        let place = held.place.subscribe();
        let mut row = None;
        // A notice is one per key: the newer word takes the older one's place
        // in the queue, and the older depositor's end is dropped unsettled.
        let waiting = state.parcels.iter_mut().find(|waiting| {
            held.parcel.notice && waiting.parcel.notice && waiting.parcel.key == held.parcel.key
        });
        let saving = match waiting {
            Some(waiting) => {
                tracing::debug!(key = %held.parcel.key, "notice replaced the one waiting under its key");
                held.seq = waiting.seq;
                if !held.parcel.saved {
                    self.shared.forget([&*waiting]);
                }
                let saving = self.shared.keep_by(&mut held, keeping, &mut row);
                *waiting = held;
                saving
            }
            None => {
                state.next_seq = state.next_seq.saturating_add(1);
                let saving = self.shared.keep_by(&mut held, keeping, &mut row);
                state.parcels.push_back(held);
                saving
            }
        };
        self.shared.wake(&mut state);
        let handed_to_wait = state
            .doors
            .iter()
            .any(|door| door.held.iter().any(|held| held.id == id));
        (
            Deposited {
                turned_away: Vec::new(),
                end: Some(rx),
                handed_to_wait,
                place: Some(place),
            },
            saving,
            row,
        )
    }

    /// How many parcels wait to be read; the ones a push carries, and the
    /// ones held back until their rows are written, are not counted.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.shared.lock().shown()
    }

    /// Whether a parcel named `key` is waiting to be read, being pushed, or
    /// held for a door.
    #[must_use]
    pub fn holds(&self, key: &str) -> bool {
        let state = self.shared.lock();
        state
            .parcels
            .iter()
            .take(state.shown())
            .any(|held| held.parcel.key == key)
            || state
                .carrying
                .iter()
                .any(|carried| carried.held.iter().any(|held| held.parcel.key == key))
            || state
                .doors
                .iter()
                .any(|door| door.held.iter().any(|held| held.parcel.key == key))
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

    /// Gives each parcel the store kept whose depositor is gone and whose key
    /// `delivery_of` names a chat delivery for a new end, so whoever answers
    /// for such parcels learns how each leaves the inbox. A parcel is adopted
    /// once; returns the adopted parcels' deliveries with their ends.
    /// Whoever adopts a parcel forgets its row itself, once it is taken or
    /// turned away, as [`Self::deposit_with_row`]'s depositor does.
    pub(crate) fn adopt_kept(
        &self,
        delivery_of: impl Fn(&str) -> Option<ChatDelivery>,
    ) -> Vec<(ChatDelivery, oneshot::Receiver<ParcelEnd>)> {
        let mut state = self.shared.lock();
        let State {
            parcels,
            carrying,
            doors,
            ..
        } = &mut *state;
        parcels
            .iter_mut()
            .chain(
                carrying
                    .iter_mut()
                    .flat_map(|carried| carried.held.iter_mut()),
            )
            .chain(doors.iter_mut().flat_map(|door| door.held.iter_mut()))
            .filter(|held| held.end.is_none() && !held.settled)
            .filter_map(|held| {
                let delivery = delivery_of(&held.parcel.key)?;
                let (tx, rx) = oneshot::channel();
                held.end = Some(tx);
                held.depositor_forgets = true;
                held.delivery = Some(delivery.clone());
                Some((delivery, rx))
            })
            .collect()
    }

    /// The most characters one answer carries: a parcel longer than this is
    /// never taken in.
    #[must_use]
    pub fn answer_limit(&self) -> usize {
        self.shared.spec.max_chars
    }

    /// Whether a wait is registered.
    #[must_use]
    pub fn has_waiter(&self) -> bool {
        self.shared.lock().waiter.is_some()
    }

    /// The answer for a CLI's start-up: `intro`, then the parcels that fit
    /// behind it, answering by `spec` or the inbox's own. The parcels it
    /// carries are held for its door.
    pub fn read_context(&self, intro: &str, spec: Option<&DeliverySpec>) -> DoorRead {
        let spec = spec.unwrap_or(&self.shared.spec);
        self.read(intro, &spec.output.context, spec.max_chars)
    }

    /// The answer for the start of a turn: the parcels that fit in one
    /// answer, held for its door, as [`Self::read_context`] answers.
    pub fn read_take(&self, spec: Option<&DeliverySpec>) -> DoorRead {
        let spec = spec.unwrap_or(&self.shared.spec);
        self.read("", &spec.output.take, spec.max_chars)
    }

    fn read(&self, intro: &str, form: &OutputForm, limit: usize) -> DoorRead {
        let mut state = self.shared.lock();
        let (taken, text) = self.shared.take_within_limit(&mut state, intro, limit);
        DoorRead {
            message: self.shared.read_message(form, &text),
            hold: self.shared.hold(&mut state, taken),
        }
    }

    /// Registers a wait for the window, replacing the one registered before:
    /// that one is answered with nothing to print and a success exit. The
    /// parcels an answer carries are held for its door.
    pub fn wait(&self) -> Wait {
        let mut state = self.shared.lock();
        if state.closed.is_some() {
            return Wait::Answered(Box::new(DoorRead::silent()));
        }
        if let Some(replaced) = state.waiter.take() {
            // The replaced wait may have ended already; then nobody reads this.
            let _ = replaced.answer.send(DoorRead::silent());
        }
        if state.shown() > 0 && self.shared.pushable() {
            let (taken, text) = self.shared.take_within(&mut state, "");
            return Wait::Answered(Box::new(DoorRead {
                message: self.shared.wake_message(&text),
                hold: self.shared.hold(&mut state, taken),
            }));
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
            .filter(|held| held.written)
            .and_then(|held| held.budget.clone())
    }

    /// Takes what one push hands over out of the inbox: everything that fits
    /// in one answer when `whole`, otherwise the oldest parcel alone. `None`
    /// when nothing is waiting. The doors do not return what a push carries.
    pub fn begin_push(&self, whole: bool) -> Option<Push> {
        let mut state = self.shared.lock();
        let held = if whole {
            self.shared.take_within(&mut state, "").0
        } else if state.shown() > 0 {
            state.parcels.pop_front().into_iter().collect()
        } else {
            Vec::new()
        };
        let first = held.first()?;
        for carried in &held {
            carried.move_to(Place::Carried);
        }
        let text = held
            .iter()
            .map(|held| held.parcel.text.as_str())
            .collect::<Vec<_>>()
            .join(SEPARATOR);
        let budget = first.budget.clone();
        let key = first.parcel.key.clone();
        state.next_push += 1;
        let id = state.next_push;
        let submitted = Arc::new(watch::channel(false).0);
        state.carrying.push(Carried {
            id,
            held,
            submitted: Arc::clone(&submitted),
            settling: None,
        });
        Some(Push {
            id,
            key,
            text,
            budget,
            submitted,
        })
    }

    /// Settles push `id` right before its first submit key, once: the rows
    /// of the kept parcels it carries are forgotten, with the chat delivery
    /// each carries recorded as taken, and the delivery mark of `mark` with
    /// them, in one transaction. Returns once that has committed -- or has
    /// failed, and is owed behind the next write that succeeds, since the key
    /// goes out regardless -- whether it settled anything (and took the mark
    /// along): not when the push carries nothing kept, or is settled already.
    pub(crate) async fn settle_push(&self, id: u64, mark: Option<String>) -> bool {
        let Some(keeper) = self.shared.keeper.as_ref() else {
            return false;
        };
        let (rows, recorded) = {
            let mut state = self.shared.lock();
            let Some(carried) = state
                .carrying
                .iter_mut()
                .find(|carried| carried.id == id && carried.settling.is_none())
            else {
                return false;
            };
            let mut rows = Vec::new();
            for held in carried.held.iter_mut().filter(|held| held.parcel.saved) {
                if held.delivery.is_none() && held.depositor_forgets {
                    continue;
                }
                rows.push(RowEnd {
                    parcel: held.kept(),
                    delivery: held.delivery.clone(),
                });
                held.settled = true;
            }
            if rows.is_empty() {
                return false;
            }
            let recorded = Arc::new(Mutex::new(Recorded::Waiting(Vec::new())));
            carried.settling = Some(Arc::clone(&recorded));
            (rows, recorded)
        };
        let settled = keeper.settle_push(
            id,
            rows,
            mark,
            Box::new(move || Recorded::commit(&recorded)),
        );
        let _ = settled.await;
        true
    }

    /// Undoes push `id`'s settling once its submit key is known not to have
    /// gone out: the rows are kept again with their chat deliveries pending,
    /// and the delivery mark of `mark` with them, before the push puts its
    /// parcels back. A write that fails is owed as the settling is. Nothing
    /// when the push was not settled.
    pub(crate) async fn unsettle_push(&self, id: u64, mark: Option<String>) {
        let Some(keeper) = self.shared.keeper.as_ref() else {
            return;
        };
        let rows = {
            let mut state = self.shared.lock();
            let Some(carried) = state
                .carrying
                .iter_mut()
                .find(|carried| carried.id == id && carried.settling.is_some())
            else {
                return;
            };
            carried.settling = None;
            carried
                .held
                .iter_mut()
                .filter_map(|held| {
                    std::mem::take(&mut held.settled).then(|| RowEnd {
                        parcel: held.kept(),
                        delivery: held.delivery.clone(),
                    })
                })
                .collect()
        };
        let _ = keeper.unsettle_push(id, rows, mark).await;
    }

    /// Whether a push is under way that has not pressed its submit key.
    #[cfg(feature = "fault-injection")]
    pub(crate) fn push_before_submit_key(&self) -> bool {
        self.shared
            .lock()
            .carrying
            .iter()
            .any(|carried| !*carried.submitted.borrow())
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
                self.shared.forget_ended(&carried.held);
                for mut held in carried.held {
                    match (&carried.settling, held.settled && held.delivery.is_some()) {
                        (Some(recorded), true) => {
                            if let Some(end) = held.end.take() {
                                Recorded::tell(recorded, end);
                            }
                        }
                        _ => held.settle(ParcelEnd::Taken),
                    }
                }
            }
            PushEnd::TurnedAway => {
                self.shared.forget_ended(&carried.held);
                for held in carried.held {
                    held.settle(ParcelEnd::TurnedAway);
                }
            }
            PushEnd::Failed => match state.closed {
                Some(closed) => {
                    for held in carried.held {
                        held.settle(closed);
                    }
                }
                None => {
                    for held in carried.held.into_iter().rev() {
                        held.move_to(Place::Waiting);
                        state.parcels.push_front(held);
                    }
                    self.shared.wake(&mut state);
                }
            },
        }
    }

    /// Withdraws the parcel named `key`, if it still waits. A waiting one is
    /// removed under the same lock the doors take it under, so once this
    /// returns [`Withdrawal::Removed`] nothing hands it to the CLI; its
    /// depositor learns nothing more from it. A parcel a door or a push holds
    /// is left to them.
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
        if let Some(removed) = state.remove_waiting(key) {
            self.shared.forget([&removed]);
            return Withdrawal::Removed;
        }
        if let Some(carried) = state
            .carrying
            .iter()
            .find(|carried| carried.held.iter().any(|held| held.parcel.key == key))
        {
            return Withdrawal::UnderWay {
                submitted: carried.submitted.subscribe(),
            };
        }
        match state
            .doors
            .iter()
            .find(|door| door.held.iter().any(|held| held.parcel.key == key))
        {
            Some(door) => Withdrawal::UnderWay {
                submitted: door.ended.subscribe(),
            },
            None => Withdrawal::Gone,
        }
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
                // A revision leaves the parcel where it shows: its row is
                // rewritten, and nothing waits on that.
                if let Some(keeper) = self.shared.keeper.as_ref().filter(|_| held.parcel.saved) {
                    keeper.keep(held.kept(), Box::new(|_| false));
                }
                true
            }
            None => false,
        }
    }

    /// Ends the inbox: every waiting parcel ends as `end`, a push under way
    /// ends that way if it fails, the registered wait is answered with
    /// nothing, and nothing more is accepted. What the store keeps is left
    /// to go with the window's row: a window whose holder was lost, or whose
    /// daemon stopped, keeps it for a later daemon.
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
            let _ = waiter.answer.send(DoorRead::silent());
        }
    }
}
