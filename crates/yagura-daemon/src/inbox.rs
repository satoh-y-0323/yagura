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
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use tokio::sync::{oneshot, watch};
use yagura_protocol::{DaemonMessage, DeliveryStream, WindowStatus, WithdrawOutcome};

use crate::parcel_store::{ChatDelivery, InboxKeeper, KeptParcel, RowEnd, Written};
use crate::waiter::StatusAt;

/// Between two parcels, and between the intro and the first parcel.
const SEPARATOR: &str = "\n\n";

/// Where a template, and a wake argument, take the text.
const TEXT_PLACEHOLDER: &str = "{text}";

/// Where a wake argument takes the CLI's session identifier.
const SESSION_PLACEHOLDER: &str = "{session}";

/// Where a template takes the name of the hook event its door ran for.
pub(crate) const EVENT_PLACEHOLDER: &str = "{event}";

/// Default value of [`DeliverySpec::max_bytes`].
pub const DEFAULT_MAX_BYTES: usize = 10_000;

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
    /// A number or a boolean.
    Scalar(serde_json::Value),
    /// An object or an array.
    Other,
}

impl PointerValue {
    /// The string, when the value is one.
    #[must_use]
    pub fn into_text(self) -> Option<String> {
        match self {
            Self::Text(text) => Some(text),
            Self::Scalar(_) | Self::Other => None,
        }
    }

    /// Whether this is the string, number or boolean `value`. An object or an
    /// array equals nothing.
    #[must_use]
    pub fn equals(&self, value: &serde_json::Value) -> bool {
        match self {
            Self::Text(text) => value.as_str() == Some(text.as_str()),
            Self::Scalar(scalar) => scalar == value,
            Self::Other => false,
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

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<(), E> {
        self.record_scalar(&PointerValue::Scalar(value.into()));
        Ok(())
    }

    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<(), E> {
        self.record_scalar(&PointerValue::Scalar(value.into()));
        Ok(())
    }

    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<(), E> {
        self.record_scalar(&PointerValue::Scalar(value.into()));
        Ok(())
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<(), E> {
        // A value that is not finite has no JSON number, so it equals nothing.
        let value = serde_json::Number::from_f64(value).map_or(PointerValue::Other, |number| {
            PointerValue::Scalar(number.into())
        });
        self.record_scalar(&value);
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
    sent: Option<Sent>,
}

/// Who sent an envelope: what the bound on the envelopes one sender leaves in
/// a window counts by, and who alone may withdraw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sender {
    /// The window of this id.
    Window(String),
    /// A caller that is no window, on the daemon connection of this number.
    External(u64),
}

impl Sender {
    /// Whether `self` and `other` count against the same bound: one window's
    /// own, or the one every caller that is no window shares.
    fn shares_bound_with(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Window(one), Self::Window(other)) => one == other,
            (Self::External(_), Self::External(_)) => true,
            (Self::Window(_), Self::External(_)) | (Self::External(_), Self::Window(_)) => false,
        }
    }
}

/// An envelope's id and its sender.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sent {
    id: String,
    sender: Sender,
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
            sent: None,
        }
    }

    /// This parcel as the envelope `id` that `sender` sent: it counts against
    /// `sender`'s bound in the window ([`Inbox::deposit`]), and only `sender`
    /// withdraws it by `id` ([`Inbox::withdraw_sent`]).
    #[must_use]
    pub fn sent_by(self, id: impl Into<String>, sender: Sender) -> Self {
        Self {
            sent: Some(Sent {
                id: id.into(),
                sender,
            }),
            ..self
        }
    }

    fn sent_by_sender(&self, sender: &Sender) -> bool {
        self.sent
            .as_ref()
            .is_some_and(|sent| sent.sender == *sender)
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
    /// the template has `{text}`, and the name of the hook event its door ran
    /// for as one JSON string wherever it has `{event}`.
    Template(String),
}

impl OutputForm {
    /// The output for `text`; nothing at all when there is no text. A
    /// `{event}` left in the template (a door that names no event, or a form
    /// not given one by [`Self::for_event`]) prints `null`.
    ///
    /// The text is inserted in a single pass, so a `{text}` inside it is
    /// never replaced.
    #[must_use]
    pub fn render(&self, text: &str) -> String {
        self.render_with_event(text, "null")
    }

    /// This form with `{event}` filled in with `event`, a hook event name
    /// ([`crate::profile_hook_events::is_hook_event_name`]).
    #[must_use]
    pub fn for_event(&self, event: &str) -> Self {
        match self {
            Self::Plain => Self::Plain,
            Self::Template(template) => {
                let named = serde_json::Value::String(event.to_string()).to_string();
                Self::Template(template.replace(EVENT_PLACEHOLDER, &named))
            }
        }
    }

    fn render_with_event(&self, text: &str, event: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        match self {
            Self::Plain => text.to_string(),
            Self::Template(template) => {
                let embedded = serde_json::Value::String(text.to_string()).to_string();
                template
                    .replace(EVENT_PLACEHOLDER, event)
                    .split(TEXT_PLACEHOLDER)
                    .collect::<Vec<_>>()
                    .join(&embedded)
            }
        }
    }

    /// The most UTF-8 bytes this form prints for `text`, whichever event its
    /// door runs for: a `{event}` left in it counts as the longest event name,
    /// so a parcel taken in by this measure fits every door that prints it.
    fn printed_len(&self, text: &str) -> usize {
        let longest = format!(
            "\"{}\"",
            "x".repeat(crate::profile_hook_events::MAX_EVENT_NAME_LEN)
        );
        self.render_with_event(text, &longest).len()
    }
}

/// One stretch of a wake argument's template.
enum WakePiece<'a> {
    Literal(&'a str),
    Session,
    Text,
}

/// `template` split at its `{session}` and `{text}`, read in a single pass.
fn wake_pieces(template: &str) -> Vec<WakePiece<'_>> {
    let mut pieces = Vec::new();
    let (mut literal, mut at) = (0, 0);
    while let Some(rest) = template.get(at..).filter(|rest| !rest.is_empty()) {
        let slot = if rest.starts_with(SESSION_PLACEHOLDER) {
            Some((WakePiece::Session, SESSION_PLACEHOLDER.len()))
        } else if rest.starts_with(TEXT_PLACEHOLDER) {
            Some((WakePiece::Text, TEXT_PLACEHOLDER.len()))
        } else {
            None
        };
        match slot {
            Some((piece, len)) => {
                if literal < at {
                    pieces.push(WakePiece::Literal(&template[literal..at]));
                }
                pieces.push(piece);
                at += len;
                literal = at;
            }
            None => at += rest.chars().next().map_or(1, char::len_utf8),
        }
    }
    if literal < template.len() {
        pieces.push(WakePiece::Literal(&template[literal..]));
    }
    pieces
}

/// `template` with `{session}` and `{text}` filled in, in one pass: what was
/// filled in is never read again, so a `{text}` inside the text stays as
/// written.
pub(crate) fn fill_wake_argument(template: &str, session: &str, text: &str) -> String {
    let mut filled = String::with_capacity(template.len() + text.len());
    for piece in wake_pieces(template) {
        filled.push_str(match piece {
            WakePiece::Literal(literal) => literal,
            WakePiece::Session => session,
            WakePiece::Text => text,
        });
    }
    filled
}

/// The UTF-8 bytes [`fill_wake_argument`] makes of `template` for `text`,
/// with the longest session identifier the daemon keeps in each `{session}`:
/// a deposit is measured before the CLI may have reported its session.
fn wake_argument_len(template: &str, text: &str) -> usize {
    wake_pieces(template)
        .iter()
        .map(|piece| match piece {
            WakePiece::Literal(literal) => literal.len(),
            // A session identifier is ASCII, one byte to a character.
            WakePiece::Session => SESSION_ID_MAX_CHARS,
            WakePiece::Text => text.len(),
        })
        .sum()
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
    /// The block of a stop that is not a continuation.
    pub block: OutputForm,
}

impl DoorOutputs {
    /// Every door prints in `form`.
    #[must_use]
    pub fn all(form: &OutputForm) -> Self {
        Self {
            context: form.clone(),
            take: form.clone(),
            wait: form.clone(),
            block: form.clone(),
        }
    }
}

/// How a window's doors answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliverySpec {
    /// The most bytes one answer carries: the UTF-8 of the output as it is
    /// printed (in its form, escaping and the intro included), and of the
    /// one wake argument that carries the text.
    pub max_bytes: usize,
    /// How the text is printed.
    pub output: DoorOutputs,
    /// The wake command's arguments, where `{text}` takes the text and
    /// `{session}` the CLI's session; empty when the CLI is not woken by
    /// command.
    pub wake_args: Vec<String>,
    /// The stream a wait that was answered by a parcel prints on.
    pub wake_stream: DeliveryStream,
    /// The exit code that wait ends with.
    pub wake_exit: u8,
}

impl Default for DeliverySpec {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            output: DoorOutputs::all(&OutputForm::Plain),
            wake_args: Vec::new(),
            wake_stream: DeliveryStream::Stderr,
            wake_exit: DEFAULT_WAKE_EXIT,
        }
    }
}

impl DeliverySpec {
    /// This spec with `{event}` in each form filled in with `event`, a hook
    /// event name ([`crate::profile_hook_events::is_hook_event_name`]).
    #[must_use]
    pub fn for_event(&self, event: &str) -> Self {
        let DoorOutputs {
            context,
            take,
            wait,
            block,
        } = &self.output;
        Self {
            output: DoorOutputs {
                context: context.for_event(event),
                take: take.for_event(event),
                wait: wait.for_event(event),
                block: block.for_event(event),
            },
            ..self.clone()
        }
    }

    /// The bytes of the widest output `text` alone is handed over in: each
    /// door's form (the context's behind `intro`) and each wake argument that
    /// carries it.
    fn widest(&self, intro: &str, text: &str) -> usize {
        let context = if intro.is_empty() {
            self.output.context.printed_len(text)
        } else {
            self.output
                .context
                .printed_len(&format!("{intro}{SEPARATOR}{text}"))
        };
        [
            context,
            self.output.take.printed_len(text),
            self.output.wait.printed_len(text),
            self.output.block.printed_len(text),
            self.push_len(text),
        ]
        .into_iter()
        .max()
        .unwrap_or(0)
    }

    /// The bytes of the widest wake argument that carries `text`; 0 when the
    /// CLI is not woken by command.
    fn push_len(&self, text: &str) -> usize {
        self.wake_args
            .iter()
            .filter(|arg| arg.contains(TEXT_PLACEHOLDER))
            .map(|arg| wake_argument_len(arg, text))
            .max()
            .unwrap_or(0)
    }
}

/// How a parcel left the inbox, as its depositor learns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParcelEnd {
    /// The window's CLI took it.
    Taken,
    /// A door handed it to the window's CLI, and its row's removal recorded
    /// the chat delivery it carried as taken: its depositor writes nothing
    /// more.
    TakenRecorded,
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

/// One parcel as the inbox holds it, and who learns how it left.
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
    /// that settles what the parcel was for, once it is taken: the inbox then
    /// leaves that row alone.
    depositor_forgets: bool,
    /// The chat delivery its row's removal records, for a depositor that
    /// forgets the row itself.
    delivery: Option<ChatDelivery>,
    /// Whether a door's confirmation has forgotten its row already (or owes
    /// that): nothing forgets it a second time.
    settled: bool,
    end: Option<oneshot::Sender<ParcelEnd>>,
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
    /// Never raised: it is dropped when the push ends, which is what a
    /// withdrawal that found the parcels here waits for.
    ended: watch::Sender<bool>,
}

/// Whether a door's confirmation has committed: until it has, the ends of the
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
    /// Its door printed what it took: none of it waits again, though the
    /// hold stays until its end is committed.
    printed: bool,
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
    /// The CLI was asked to register a wait again and has not answered yet:
    /// kept under the lock the wait is, so an ask is decided against it.
    asked: bool,
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

    /// How many parcels the inbox holds, waiting or carried, that count
    /// against the same bound as `sender`.
    fn held_against(&self, sender: &Sender) -> usize {
        self.parcels
            .iter()
            .chain(self.carrying.iter().flat_map(|carried| carried.held.iter()))
            .chain(self.doors.iter().flat_map(|door| door.held.iter()))
            .filter(|held| {
                held.parcel
                    .sent
                    .as_ref()
                    .is_some_and(|sent| sent.sender.shares_bound_with(sender))
            })
            .count()
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
    /// The most parcels one sender's envelopes take in the inbox at once.
    sender_bound: usize,
    spec: DeliverySpec,
    /// The most the window's context prints ahead of the parcels.
    intro: String,
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

    /// [`Self::forget`] for parcels that were taken: a parcel whose depositor
    /// forgets its row itself keeps it until then.
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

    /// Whether `text` alone is handed over whole in every output the window
    /// prints it in.
    fn fits(&self, text: &str) -> bool {
        self.spec.widest(&self.intro, text) <= self.spec.max_bytes
    }

    /// The parcels one answer to a wait carries, as [`Self::take_within`]
    /// takes them.
    fn take_for_wait(&self, state: &mut State) -> (Vec<Held>, String) {
        let form = &self.spec.output.wait;
        self.take_within(state, "", self.spec.max_bytes, |text| {
            form.printed_len(text)
        })
    }

    /// Takes the parcels that fit within `limit` bytes once `printed_len`
    /// measures their text, oldest first, behind `intro` when there is one. A
    /// parcel is never cut: the first one that does not fit stays for the
    /// next answer.
    fn take_within(
        &self,
        state: &mut State,
        intro: &str,
        limit: usize,
        printed_len: impl Fn(&str) -> usize,
    ) -> (Vec<Held>, String) {
        let mut text = longest_prefix_within(intro, limit, &printed_len).to_string();
        let mut taken = Vec::new();
        while let Some(front) = state.parcels.front().filter(|held| held.written) {
            let separator = if text.is_empty() { "" } else { SEPARATOR };
            let candidate = format!("{text}{separator}{}", front.parcel.text);
            if printed_len(&candidate) > limit {
                break;
            }
            let Some(held) = state.parcels.pop_front() else {
                break;
            };
            text = candidate;
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
        let (taken, text) = self.take_for_wait(state);
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
            printed: false,
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
            door.printed = true;
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
            let settled = keeper.settle_push(rows, Box::new(move || Recorded::commit(&committed)));
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
    /// Whether the door prints anything for this read.
    #[must_use]
    pub fn prints(&self) -> bool {
        matches!(&self.message, DaemonMessage::Delivery { output, .. } if !output.is_empty())
    }

    /// The answer that makes a hook print nothing and succeed, taking nothing.
    #[must_use]
    pub fn silent() -> Self {
        Self {
            message: silence(),
            hold: DoorHold::empty(),
        }
    }

    /// The answer that makes a hook print `output` and succeed, taking
    /// nothing from the inbox.
    #[must_use]
    pub fn printing(output: String) -> Self {
        Self {
            message: DaemonMessage::Delivery {
                output,
                stream: DeliveryStream::Stdout,
                exit_code: 0,
                hook_timeout_sec: None,
            },
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

    /// Whether it holds a parcel.
    #[must_use]
    pub fn holds(&self) -> bool {
        self.id.is_some()
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
    /// Whether it was turned away because its sender's envelopes already
    /// take as many places in the inbox as one sender may.
    pub sender_at_bound: bool,
}

/// A push under way: what it carries.
pub struct Push {
    /// Names the push to [`Inbox::finish_push`].
    pub id: u64,
    /// The key of the first parcel it carries.
    pub key: String,
    /// The text the push hands over.
    pub text: String,
}

/// How a push ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushEnd {
    /// The CLI has what the push carried.
    Taken,
    /// Nothing reached the CLI: the parcels go back in front of the inbox.
    Failed,
}

/// What withdrawing a parcel came to.
#[derive(Debug, Clone)]
pub enum Withdrawal {
    /// It was waiting and is out of the inbox: it never reaches the CLI.
    Removed,
    /// A door or a push holds it: it ends with that door or push, and goes
    /// back to waiting when they fail.
    UnderWay {
        /// Never turns true; closes when the door or push ends, which is when
        /// the parcel is waiting again or ended.
        ended: watch::Receiver<bool>,
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
    /// and at most [`crate::config::DEFAULT_MAX_QUEUED_PER_SENDER`] of one
    /// sender's envelopes, answering by `spec`
    /// and pushing only while `status` allows it.
    #[must_use]
    pub fn new(capacity: usize, spec: DeliverySpec, status: watch::Receiver<StatusAt>) -> Self {
        Self::kept(
            capacity,
            crate::config::DEFAULT_MAX_QUEUED_PER_SENDER,
            spec,
            "",
            status,
            None,
            Vec::new(),
        )
    }

    /// [`Self::new`] holding at most `sender_bound` of one sender's envelopes,
    /// for a window whose context prints at most `intro` ahead
    /// of the parcels, keeping every change through `keeper` and holding
    /// `kept` first: the parcels the store kept for the window, oldest first,
    /// whose depositors are gone. A kept parcel no answer carries whole is
    /// dropped, as a deposit of it would have been turned away.
    pub(crate) fn kept(
        capacity: usize,
        sender_bound: usize,
        spec: DeliverySpec,
        intro: &str,
        status: watch::Receiver<StatusAt>,
        keeper: Option<InboxKeeper>,
        kept: Vec<KeptParcel>,
    ) -> Self {
        let mut state = State::default();
        let mut too_long = Vec::new();
        for parcel in kept {
            state.next_seq = state.next_seq.max(parcel.seq.saturating_add(1));
            if spec.widest(intro, &parcel.text) > spec.max_bytes {
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
                    sent: None,
                },
                seq: parcel.seq,
                id,
                written: true,
                wakes: false,
                depositor_forgets: false,
                delivery: None,
                settled: false,
                end: None,
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
                sender_bound,
                spec,
                intro: intro.to_string(),
                status,
                keeper,
                #[cfg(test)]
                before_withdraw: Mutex::new(None),
            }),
        }
    }

    /// Adds `parcel` and returns the parcels the inbox turned away: `parcel`
    /// itself when the inbox is full of other parcels, has ended, its
    /// sender's envelopes already take as many places as one sender may
    /// (waiting or carried; see [`Parcel::sent_by`]), or `parcel`
    /// alone is not carried whole by some output the window prints it in
    /// (each door's, and the wake command's). Nothing already inside is
    /// pushed out, except that a notice replaces the notice waiting under its
    /// key, which is not returned (see [`Parcel::notice`]). A registered wait
    /// is answered when the window can be pushed to.
    pub fn deposit(&self, parcel: Parcel) -> Vec<Parcel> {
        self.deposit_as(parcel, false).0.turned_away
    }

    /// [`Self::deposit`], telling the depositor how the parcel leaves the
    /// inbox. A notice that replaces another leaves the replaced one's end
    /// dropped unsettled. A kept parcel shows once its row is written, and one
    /// whose row cannot be written leaves without ever showing. The depositor
    /// wakes the window's agent: one held back is woken for again once it
    /// shows.
    pub fn deposit_tracked(&self, parcel: Parcel) -> Deposited {
        self.deposit_as(parcel, true).0
    }

    /// [`Self::deposit_tracked`], with [`Saving`] telling the depositor how
    /// the parcel's row came out, for a depositor that answers for it being
    /// kept.
    pub(crate) fn deposit_saved(&self, parcel: Parcel) -> (Deposited, Saving) {
        self.deposit_as(parcel, true)
    }

    /// [`Self::deposit_tracked`] for a depositor that writes the parcel's row
    /// itself, in one transaction with `delivery`, the chat delivery the
    /// parcel carries. The row to write comes back with the [`Unwritten`]
    /// that holds the parcel back until the depositor tells how the write
    /// came out; `None` when the inbox keeps nothing. The parcel's row is
    /// forgotten by its depositor too, once it is taken, but for a door's
    /// confirmation, which records `delivery` with it
    /// ([`ParcelEnd::TakenRecorded`]).
    pub(crate) fn deposit_with_row(
        &self,
        parcel: Parcel,
        delivery: ChatDelivery,
    ) -> (Deposited, Option<(KeptParcel, Unwritten)>) {
        let (deposited, _, row) = self.deposit_by(parcel, true, Keeping::Depositor(delivery));
        (deposited, row)
    }

    fn deposit_as(&self, parcel: Parcel, wakes: bool) -> (Deposited, Saving) {
        let (deposited, saving, _) = self.deposit_by(parcel, wakes, Keeping::Keeper);
        (deposited, saving)
    }

    fn deposit_by(
        &self,
        parcel: Parcel,
        wakes: bool,
        keeping: Keeping,
    ) -> (Deposited, Saving, Option<(KeptParcel, Unwritten)>) {
        let refused = |parcel, sender_at_bound| {
            (
                Deposited {
                    turned_away: vec![parcel],
                    end: None,
                    handed_to_wait: false,
                    place: None,
                    sender_at_bound,
                },
                Saving::ready(),
                None,
            )
        };
        if !self.shared.fits(&parcel.text) {
            return refused(parcel, false);
        }
        let mut state = self.shared.lock();
        // A notice is one per key, so it bounds itself; it is never turned
        // away, and never counted, for the room other parcels take.
        let full = !parcel.notice && state.waiting_others() >= self.shared.capacity;
        if state.closed.is_some() || full {
            return refused(parcel, false);
        }
        // Checked under the lock the parcel goes in under, so two deposits of
        // one sender never both take its last place.
        if let Some(sent) = &parcel.sent
            && state.held_against(&sent.sender) >= self.shared.sender_bound
        {
            return refused(parcel, true);
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
                sender_at_bound: false,
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

    /// Whether a parcel of `text` is carried whole by every output the window
    /// prints it in: one that is not is never taken in.
    #[must_use]
    pub fn fits(&self, text: &str) -> bool {
        self.shared.fits(text)
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
        self.read(intro, &spec.output.context, spec.max_bytes)
    }

    /// The answer for the start of a turn: the parcels that fit in one
    /// answer, held for its door, as [`Self::read_context`] answers.
    pub fn read_take(&self, spec: Option<&DeliverySpec>) -> DoorRead {
        let spec = spec.unwrap_or(&self.shared.spec);
        self.read("", &spec.output.take, spec.max_bytes)
    }

    /// The block of a stop: the parcels that fit in one answer, held for its
    /// door, as [`Self::read_context`] answers. It never waits, and it reads
    /// in any status, as the other doors do: the CLI's own hook asks.
    pub fn read_block(&self, spec: Option<&DeliverySpec>) -> DoorRead {
        let spec = spec.unwrap_or(&self.shared.spec);
        self.read("", &spec.output.block, spec.max_bytes)
    }

    /// How this inbox's window answers at its doors.
    #[must_use]
    pub fn spec(&self) -> &DeliverySpec {
        &self.shared.spec
    }

    fn read(&self, intro: &str, form: &OutputForm, limit: usize) -> DoorRead {
        let mut state = self.shared.lock();
        let (taken, text) = self
            .shared
            .take_within(&mut state, intro, limit, |text| form.printed_len(text));
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
        // A wait is the answer to an ask.
        state.asked = false;
        if let Some(replaced) = state.waiter.take() {
            // The replaced wait may have ended already; then nobody reads this.
            let _ = replaced.answer.send(DoorRead::silent());
        }
        if state.shown() > 0 && self.shared.pushable() {
            let (taken, text) = self.shared.take_for_wait(&mut state);
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

    /// Whether the window's CLI is to be asked to register a wait again now,
    /// and marks it asked when it is: a parcel shows that can be pushed, no
    /// wait is registered, and no ask stands unanswered. Decided under the
    /// lock a wait registers under, so a wait that registers meanwhile is
    /// seen.
    #[must_use]
    pub fn ask_due(&self) -> bool {
        let mut state = self.shared.lock();
        if state.asked
            || state.waiter.is_some()
            || state.closed.is_some()
            || state.shown() == 0
            || !self.shared.pushable()
        {
            return false;
        }
        state.asked = true;
        true
    }

    /// Takes back the ask that stands: the CLI answered it (a door arrived,
    /// or the window's status changed), or it could not be made. Returns
    /// whether one stood.
    pub fn clear_ask(&self) -> bool {
        std::mem::take(&mut self.shared.lock().asked)
    }

    /// Answers the registered wait with what is waiting, when the window can
    /// be pushed to now.
    pub fn wake_waiter(&self) {
        let mut state = self.shared.lock();
        self.shared.wake(&mut state);
    }

    /// Takes what one push hands over out of the inbox: everything that fits
    /// in one wake argument. `None` when nothing is waiting. The doors do not
    /// return what a push carries.
    pub fn begin_push(&self) -> Option<Push> {
        let mut state = self.shared.lock();
        let spec = &self.shared.spec;
        let held = self
            .shared
            .take_within(&mut state, "", spec.max_bytes, |text| spec.push_len(text))
            .0;
        let first = held.first()?;
        for carried in &held {
            carried.move_to(Place::Carried);
        }
        let text = held
            .iter()
            .map(|held| held.parcel.text.as_str())
            .collect::<Vec<_>>()
            .join(SEPARATOR);
        let key = first.parcel.key.clone();
        state.next_push += 1;
        let id = state.next_push;
        state.carrying.push(Carried {
            id,
            held,
            ended: watch::channel(false).0,
        });
        Some(Push { id, key, text })
    }

    /// Ends push `id`: what it carried is taken, or goes back in front of the
    /// inbox.
    pub fn finish_push(&self, id: u64, end: PushEnd) {
        let mut state = self.shared.lock();
        let Some(position) = state.carrying.iter().position(|carried| carried.id == id) else {
            return;
        };
        let carried = state.carrying.remove(position);
        match end {
            PushEnd::Taken => {
                self.shared.forget_ended(&carried.held);
                for held in carried.held {
                    held.settle(ParcelEnd::Taken);
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
                ended: carried.ended.subscribe(),
            };
        }
        match state
            .doors
            .iter()
            .find(|door| door.held.iter().any(|held| held.parcel.key == key))
        {
            Some(door) => Withdrawal::UnderWay {
                ended: door.ended.subscribe(),
            },
            None => Withdrawal::Gone,
        }
    }

    /// Withdraws the envelope `id` for `asker`, answered from the envelopes
    /// the inbox holds now: [`WithdrawOutcome::Withdrawn`] when it waited and
    /// is out, under the same lock the doors take it under, so nothing hands
    /// it to the CLI after that; [`WithdrawOutcome::UnderWay`] when a door or
    /// a push carries it; [`WithdrawOutcome::NotWaiting`] when the inbox holds
    /// no envelope `id` that `asker` sent, or the inbox ended.
    pub fn withdraw_sent(&self, id: &str, asker: &Sender) -> WithdrawOutcome {
        let mut state = self.shared.lock();
        if state.closed.is_some() {
            return WithdrawOutcome::NotWaiting;
        }
        let is_it = |held: &Held| {
            held.parcel
                .sent
                .as_ref()
                .is_some_and(|sent| sent.id == id && sent.sender == *asker)
        };
        if let Some(position) = state.parcels.iter().position(is_it)
            && let Some(removed) = state.parcels.remove(position)
        {
            self.shared.forget([&removed]);
            return WithdrawOutcome::Withdrawn;
        }
        let carried = state
            .carrying
            .iter()
            .flat_map(|carried| carried.held.iter())
            .chain(
                state
                    .doors
                    .iter()
                    .filter(|door| !door.printed)
                    .flat_map(|door| door.held.iter()),
            )
            .any(is_it);
        if carried {
            WithdrawOutcome::UnderWay
        } else {
            WithdrawOutcome::NotWaiting
        }
    }

    /// Withdraws every envelope the window `window_id` sent that still waits,
    /// once that window ended: nobody is left to answer them. One a door or a
    /// push carries is left to it, and waits again if they fail.
    pub fn withdraw_sent_by(&self, window_id: &str) {
        let sender = Sender::Window(window_id.to_string());
        let mut state = self.shared.lock();
        let (gone, staying): (VecDeque<Held>, VecDeque<Held>) = std::mem::take(&mut state.parcels)
            .into_iter()
            .partition(|held| held.parcel.sent_by_sender(&sender));
        state.parcels = staying;
        self.shared.forget(&gone);
    }

    /// The ids of the envelopes `sender` sent that wait to be read, oldest
    /// first.
    #[must_use]
    pub fn waiting_sent_by(&self, sender: &Sender) -> Vec<String> {
        let state = self.shared.lock();
        state
            .parcels
            .iter()
            .take(state.shown())
            .filter_map(|held| {
                held.parcel
                    .sent
                    .as_ref()
                    .filter(|sent| sent.sender == *sender)
                    .map(|sent| sent.id.clone())
            })
            .collect()
    }

    /// Replaces the text of the waiting parcel named `key`. `false` when no
    /// such parcel waits, or the new text would be turned away by
    /// [`Self::deposit`].
    pub fn revise(&self, key: &str, text: String) -> bool {
        if !self.shared.fits(&text) {
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

/// The longest start of `text`, cut between characters, that `printed_len`
/// measures within `limit`: an intro is never more than one answer carries.
fn longest_prefix_within(text: &str, limit: usize, printed_len: impl Fn(&str) -> usize) -> &str {
    if printed_len(text) <= limit {
        return text;
    }
    let ends: Vec<usize> = text.char_indices().map(|(at, _)| at).collect();
    // `ends[0]` is 0, and the empty text prints nothing, so it always fits.
    let fitting = ends.partition_point(|&end| printed_len(&text[..end]) <= limit);
    &text[..ends[fitting.saturating_sub(1)]]
}
