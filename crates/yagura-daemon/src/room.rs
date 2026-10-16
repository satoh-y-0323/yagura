//! The pure functions and types behind the chat rooms: mention
//! resolution, the repetition / monologue / turn-budget halt rules, the
//! silence candidate rule, the chat envelope, and the `[chat]` limits'
//! defaults and range check.
//!
//! Nothing here touches a PTY, a store or the wire protocol, so every rule
//! can be checked with plain values, independent of a window, a connection
//! or a clock.

use crate::handoff;
use yagura_protocol::UnknownReason;

// =====================================================================
// Mention resolution
// =====================================================================

/// One roster entry a mention can resolve against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterEntry {
    /// The window's id, the daemon's addressing key.
    pub window_id: String,
    /// The window's display name, matched case-insensitively against `@name`.
    pub name: String,
}

/// The result of scanning a message body for `@` mentions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mentions {
    /// Resolved window ids, deduplicated, in first-occurrence order. Never
    /// contains the message's own `self_window_id`.
    pub targets: Vec<String>,
    /// Whether the reserved `@user` mention appeared anywhere in the text.
    pub user: bool,
    /// Whether the reserved `@parent` mention appeared anywhere in the text.
    /// Who the parent is depends on the plan ledger and the window registry,
    /// so the caller resolves it; this only records that it was asked for.
    pub parent: bool,
    /// Raw mention text (quotes and `@` stripped) for every mention that did
    /// not resolve to exactly one roster entry, deduplicated, in
    /// first-occurrence order.
    pub unresolved: Vec<String>,
}

/// Whether `c` can appear inside an unquoted mention token: letters, digits,
/// `-` and `_` (the separator characters two names may differ by).
fn is_mention_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_'
}

/// Scans `text` for `@` mentions, returning each one's raw content (quotes
/// and `@` stripped) in the order it appears, duplicates included. A mention
/// is only recognized where the `@` is not itself glued to a preceding word
/// character (so an email-shaped `name@host` is left alone).
fn extract_mention_tokens(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let is_at = chars[i] == '@';
        let glued = i > 0 && is_mention_word_char(chars[i - 1]);
        if is_at && !glued && i + 1 < chars.len() {
            if chars[i + 1] == '"' {
                let mut j = i + 2;
                while j < chars.len() && chars[j] != '"' {
                    j += 1;
                }
                if j < chars.len() {
                    tokens.push(chars[i + 2..j].iter().collect());
                    i = j + 1;
                    continue;
                }
            } else if is_mention_word_char(chars[i + 1]) {
                let mut j = i + 1;
                while j < chars.len() && is_mention_word_char(chars[j]) {
                    j += 1;
                }
                tokens.push(chars[i + 1..j].iter().collect());
                i = j;
                continue;
            }
        }
        i += 1;
    }
    tokens
}

/// Normalizes mention text for roster matching: lowercased, with `-` and `_`
/// treated as the same separator.
fn normalize_mention(s: &str) -> String {
    s.to_lowercase().replace('-', "_")
}

/// Resolves every `@` mention in `text` against `roster`.
///
/// Matching is case-insensitive and treats `-`/`_` as equivalent. A mention
/// is tried first against every entry's `name`, then, only if no name
/// matched, against every entry's `window_id`; a mention matching more than
/// one entry is reported in `unresolved` rather than guessed at. `@user`
/// (any case) sets [`Mentions::user`] and never resolves as a name or id,
/// even for a roster entry literally named "user" -- such a window is only
/// reachable by its id. `@parent` is reserved the same way and sets
/// [`Mentions::parent`] instead. `@all` is a no-op: it is recognized and dropped,
/// leaving no trace in any field. A mention that resolves to
/// `self_window_id` is dropped from `targets` without becoming unresolved.
///
/// # Examples
///
/// ```
/// use yagura_daemon::room::{RosterEntry, parse_mentions};
///
/// let roster = vec![RosterEntry { window_id: "w-b".to_string(), name: "Bob".to_string() }];
/// let mentions = parse_mentions("ping @Bob please", &roster, "w-a");
/// assert_eq!(mentions.targets, vec!["w-b".to_string()]);
/// ```
#[must_use]
pub fn parse_mentions(text: &str, roster: &[RosterEntry], self_window_id: &str) -> Mentions {
    let mut targets: Vec<String> = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();
    let mut user = false;
    let mut parent = false;

    for raw in extract_mention_tokens(text) {
        let lower = raw.to_lowercase();
        if lower == "user" {
            user = true;
            continue;
        }
        if lower == "parent" {
            parent = true;
            continue;
        }
        if lower == "all" {
            continue;
        }

        let normalized = normalize_mention(&raw);
        let name_matches: Vec<&RosterEntry> = roster
            .iter()
            .filter(|entry| normalize_mention(&entry.name) == normalized)
            .collect();

        let resolved = if name_matches.len() == 1 {
            Some(name_matches[0].window_id.clone())
        } else if name_matches.is_empty() {
            let id_matches: Vec<&RosterEntry> = roster
                .iter()
                .filter(|entry| normalize_mention(&entry.window_id) == normalized)
                .collect();
            if id_matches.len() == 1 {
                Some(id_matches[0].window_id.clone())
            } else {
                None
            }
        } else {
            None
        };

        match resolved {
            Some(id) if id == self_window_id => {}
            Some(id) => {
                if !targets.contains(&id) {
                    targets.push(id);
                }
            }
            None => {
                if !unresolved.contains(&raw) {
                    unresolved.push(raw);
                }
            }
        }
    }

    Mentions {
        targets,
        user,
        parent,
        unresolved,
    }
}

// =====================================================================
// The repetition / monologue / turn-budget halt rules
// =====================================================================

/// Who posted one [`HistoryEntry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Speaker {
    /// An AI window, identified by its id.
    Window(String),
    /// The human user.
    User,
    /// A caller outside the daemon's window registry.
    External,
    /// A system-generated entry (e.g. a silence notice). Passes through the
    /// halt rules without occupying a slot and without counting as a
    /// speaker change.
    System,
}

/// One posted message, as the halt rules need to see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// Who posted this entry.
    pub speaker: Speaker,
    /// Whether this entry's mentions included the reserved `@user`.
    pub mentions_user: bool,
    /// This entry's resolved mention targets (order-independent for
    /// position comparison).
    pub targets: Vec<String>,
    /// The message text, compared by similarity for the repetition rule.
    pub body: String,
}

/// Why [`evaluate_limits`] halted the room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltReason {
    /// The same speaker, target set and `@user` flag posted near-identical
    /// bodies too many times within the repetition window.
    Repetition,
    /// The same speaker posted too many consecutive messages.
    Monologue,
    /// Too many AI messages landed since the last user message.
    TurnBudget,
}

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

/// FNV-1a over `bytes`, the per-token hash the repetition similarity check is
/// built on.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Normalizes text for the repetition similarity check: full-width Latin and
/// punctuation folded to ASCII, ideographic space folded to a plain space,
/// lowercased, anything but ASCII alphanumerics and spaces dropped, and runs
/// of whitespace collapsed to one space.
fn normalize_for_simhash(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\u{3000}' => folded.push(' '),
            '\u{FF01}'..='\u{FF5E}' => match char::from_u32(ch as u32 - 0xFEE0) {
                Some(shifted) => folded.push(shifted),
                None => folded.push(ch),
            },
            other => folded.push(other),
        }
    }
    let lowered = folded.to_lowercase();
    let filtered: String = lowered
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == ' ')
        .collect();
    filtered.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A 64-bit simhash of `text`'s bag of words: each token votes on every bit
/// of its FNV-1a hash, and a bit is set in the result iff its votes are
/// positive. Word order does not affect the result, so a reordering of the
/// same words produces the same hash.
fn simhash64(text: &str) -> u64 {
    let normalized = normalize_for_simhash(text);
    let mut votes = [0i64; 64];
    for token in normalized.split_whitespace() {
        let hash = fnv1a_64(token.as_bytes());
        for (bit, vote) in votes.iter_mut().enumerate() {
            if hash & (1u64 << bit) == 0 {
                *vote -= 1;
            } else {
                *vote += 1;
            }
        }
    }
    let mut result: u64 = 0;
    for (bit, vote) in votes.iter().enumerate() {
        if *vote > 0 {
            result |= 1u64 << bit;
        }
    }
    result
}

/// The Hamming distance between two texts' [`simhash64`] values: 0 for
/// identical bags of words, growing with how unrelated the texts are.
fn simhash_distance(a: &str, b: &str) -> u32 {
    (simhash64(a) ^ simhash64(b)).count_ones()
}

/// Whether `entry` is a [`Speaker::System`] entry -- excluded from every halt
/// rule's window and streak accounting.
fn is_system(entry: &HistoryEntry) -> bool {
    matches!(entry.speaker, Speaker::System)
}

/// The repetition position key's target component: `targets`, sorted so
/// listing the same set in a different order does not create a new key.
fn sorted_targets(entry: &HistoryEntry) -> Vec<String> {
    let mut targets = entry.targets.clone();
    targets.sort();
    targets
}

/// Whether `a` and `b` share a repetition position: same speaker, same
/// sorted target set, same `@user` flag.
fn same_position(a: &HistoryEntry, b: &HistoryEntry) -> bool {
    a.speaker == b.speaker
        && a.mentions_user == b.mentions_user
        && sorted_targets(a) == sorted_targets(b)
}

/// Whether the message under judgement (`history`'s last entry) trips the
/// repetition rule: among the trailing `repetition_window` non-system
/// entries, at least `repetition_count` of them share its position and have
/// a body within `repetition_distance` of its body (the entry itself always
/// counts, at distance 0).
fn check_repetition(history: &[HistoryEntry], limits: &ChatLimits) -> bool {
    let Some(judged) = history.last() else {
        return false;
    };
    let non_system: Vec<&HistoryEntry> = history.iter().filter(|e| !is_system(e)).collect();
    let window_len = (limits.repetition_window as usize).min(non_system.len());
    let window_start = non_system.len() - window_len;
    let window = &non_system[window_start..];

    let count = window
        .iter()
        .filter(|entry| {
            same_position(entry, judged)
                && u64::from(simhash_distance(&entry.body, &judged.body))
                    <= limits.repetition_distance
        })
        .count() as u64;

    count >= limits.repetition_count
}

/// Whether the trailing run of same-speaker non-system entries (ending at
/// `history`'s last entry) reaches `monologue_max`. A system entry passes
/// through without breaking the run.
fn check_monologue(history: &[HistoryEntry], limits: &ChatLimits) -> bool {
    let non_system: Vec<&HistoryEntry> = history.iter().filter(|e| !is_system(e)).collect();
    let Some(last) = non_system.last() else {
        return false;
    };
    let mut streak: u64 = 0;
    for entry in non_system.iter().rev() {
        if entry.speaker != last.speaker {
            break;
        }
        streak += 1;
    }
    streak >= limits.monologue_max
}

/// Whether the number of non-user, non-system entries since the last
/// [`Speaker::User`] entry reaches `turn_message_max`. [`Speaker::External`]
/// counts the same as a window.
fn check_turn_budget(history: &[HistoryEntry], limits: &ChatLimits) -> bool {
    let non_system: Vec<&HistoryEntry> = history.iter().filter(|e| !is_system(e)).collect();
    let mut count: u64 = 0;
    for entry in non_system.iter().rev() {
        if matches!(entry.speaker, Speaker::User) {
            break;
        }
        count += 1;
    }
    count >= limits.turn_message_max
}

/// Evaluates whether the message under judgement -- `history`'s last entry
/// -- should halt the room, checking repetition, then monologue, then the
/// turn budget (the three rules are independent; a history can only trip
/// more than one at once if it is built to, which none of this module's
/// callers do).
///
/// # Examples
///
/// ```
/// use yagura_daemon::room::{ChatLimits, HaltReason, HistoryEntry, Speaker, evaluate_limits};
///
/// let limits = ChatLimits {
///     monologue_max: 2,
///     ..ChatLimits::default()
/// };
/// let history = vec![
///     HistoryEntry {
///         speaker: Speaker::Window("w-a".to_string()),
///         mentions_user: false,
///         targets: Vec::new(),
///         body: "one".to_string(),
///     },
///     HistoryEntry {
///         speaker: Speaker::Window("w-a".to_string()),
///         mentions_user: false,
///         targets: Vec::new(),
///         body: "two".to_string(),
///     },
/// ];
/// assert_eq!(evaluate_limits(&history, &limits), Some(HaltReason::Monologue));
/// ```
#[must_use]
pub fn evaluate_limits(history: &[HistoryEntry], limits: &ChatLimits) -> Option<HaltReason> {
    if history.is_empty() {
        return None;
    }
    if check_repetition(history, limits) {
        return Some(HaltReason::Repetition);
    }
    if check_monologue(history, limits) {
        return Some(HaltReason::Monologue);
    }
    if check_turn_budget(history, limits) {
        return Some(HaltReason::TurnBudget);
    }
    None
}

// =====================================================================
// The silence candidate rule
// =====================================================================

/// How a chat message landed in one addressed window: the vocabulary the
/// store persists and `read_chat` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatDeliveryState {
    /// Recorded when the message was appended, before any delivery ran.
    Pending,
    /// Written, and the window was seen taking it: its composer emptied
    /// after the submit key, or it started working.
    Confirmed,
    /// Written, but the window was not seen taking it.
    Unconfirmed,
    /// Written, but the window's reaction could not be observed at all.
    NotObserved,
    /// Never written to the window.
    NotWritten,
    /// Writing to the window failed.
    WriteFailed,
    /// The destination could not be delivered to: it is gone, or its status
    /// does not accept a write.
    UndeliveredUnknown,
    /// Withheld because the room was halted.
    UndeliveredHalted,
    /// Withheld because the chat delivery budget was already spent.
    UndeliveredExhausted,
}

impl ChatDeliveryState {
    /// The wire string this state is persisted and reported as.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Confirmed => "confirmed",
            Self::Unconfirmed => "unconfirmed",
            Self::NotObserved => "not_observed",
            Self::NotWritten => "not_written",
            Self::WriteFailed => "write_failed",
            Self::UndeliveredUnknown => "undelivered_unknown",
            Self::UndeliveredHalted => "undelivered_halted",
            Self::UndeliveredExhausted => "undelivered_exhausted",
        }
    }

    /// The state `value` spells, or `None` when it spells none of them: a
    /// string this vocabulary does not contain is never guessed at.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "confirmed" => Some(Self::Confirmed),
            "unconfirmed" => Some(Self::Unconfirmed),
            "not_observed" => Some(Self::NotObserved),
            "not_written" => Some(Self::NotWritten),
            "write_failed" => Some(Self::WriteFailed),
            "undelivered_unknown" => Some(Self::UndeliveredUnknown),
            "undelivered_halted" => Some(Self::UndeliveredHalted),
            "undelivered_exhausted" => Some(Self::UndeliveredExhausted),
            _ => None,
        }
    }
}

/// The recorded outcome of one destination's delivery: one value, of which only the
/// status-unknown arm can carry the reason the destination's status was unknown.
/// The other eight arms share their names with [`ChatDeliveryState`], which
/// [`Self::state`] projects onto -- the same shape `yagura_protocol::DetectedStatus` uses for
/// the state/reason pair it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// See [`ChatDeliveryState::Pending`].
    Pending,
    /// See [`ChatDeliveryState::Confirmed`].
    Confirmed,
    /// See [`ChatDeliveryState::Unconfirmed`].
    Unconfirmed,
    /// See [`ChatDeliveryState::NotObserved`].
    NotObserved,
    /// See [`ChatDeliveryState::NotWritten`].
    NotWritten,
    /// See [`ChatDeliveryState::WriteFailed`].
    WriteFailed,
    /// See [`ChatDeliveryState::UndeliveredUnknown`]. The only arm that can carry a reason;
    /// `None` when nothing was observed about the destination.
    UndeliveredUnknown(Option<UnknownReason>),
    /// See [`ChatDeliveryState::UndeliveredHalted`].
    UndeliveredHalted,
    /// See [`ChatDeliveryState::UndeliveredExhausted`].
    UndeliveredExhausted,
}

impl DeliveryOutcome {
    /// The delivery state this outcome projects onto.
    #[must_use]
    pub fn state(self) -> ChatDeliveryState {
        match self {
            Self::Pending => ChatDeliveryState::Pending,
            Self::Confirmed => ChatDeliveryState::Confirmed,
            Self::Unconfirmed => ChatDeliveryState::Unconfirmed,
            Self::NotObserved => ChatDeliveryState::NotObserved,
            Self::NotWritten => ChatDeliveryState::NotWritten,
            Self::WriteFailed => ChatDeliveryState::WriteFailed,
            Self::UndeliveredUnknown(_) => ChatDeliveryState::UndeliveredUnknown,
            Self::UndeliveredHalted => ChatDeliveryState::UndeliveredHalted,
            Self::UndeliveredExhausted => ChatDeliveryState::UndeliveredExhausted,
        }
    }

    /// The reason the destination's status was unknown, or `None` for every arm but
    /// [`Self::UndeliveredUnknown`] -- including its own reason-less form.
    #[must_use]
    pub fn unknown_reason(self) -> Option<UnknownReason> {
        match self {
            Self::UndeliveredUnknown(reason) => reason,
            Self::Pending
            | Self::Confirmed
            | Self::Unconfirmed
            | Self::NotObserved
            | Self::NotWritten
            | Self::WriteFailed
            | Self::UndeliveredHalted
            | Self::UndeliveredExhausted => None,
        }
    }

    /// Folds a persisted `(state, reason)` pair back into one outcome: the reason survives
    /// only when `state` is [`ChatDeliveryState::UndeliveredUnknown`], the one arm able to
    /// hold it. This is the only reconstruction point a persistence reader needs.
    #[must_use]
    pub fn from_parts(state: ChatDeliveryState, reason: Option<UnknownReason>) -> Self {
        match state {
            ChatDeliveryState::Pending => Self::Pending,
            ChatDeliveryState::Confirmed => Self::Confirmed,
            ChatDeliveryState::Unconfirmed => Self::Unconfirmed,
            ChatDeliveryState::NotObserved => Self::NotObserved,
            ChatDeliveryState::NotWritten => Self::NotWritten,
            ChatDeliveryState::WriteFailed => Self::WriteFailed,
            ChatDeliveryState::UndeliveredUnknown => Self::UndeliveredUnknown(reason),
            ChatDeliveryState::UndeliveredHalted => Self::UndeliveredHalted,
            ChatDeliveryState::UndeliveredExhausted => Self::UndeliveredExhausted,
        }
    }
}

/// The evidence [`is_silence_candidate`] weighs for one posted message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SilenceCheck {
    /// One delivery state per addressed window.
    pub deliveries: Vec<ChatDeliveryState>,
    /// Whether any addressed window has spoken in the room since.
    pub replied_since: bool,
    /// How long has elapsed since the message was posted, in milliseconds.
    pub elapsed_ms: u64,
    /// Whether the room is currently halted.
    pub halted: bool,
    /// Whether this message was already noted as silent.
    pub silence_noted: bool,
}

/// Whether `check` describes a message that should be flagged for silence:
/// it actually landed somewhere ([`ChatDeliveryState::Confirmed`],
/// [`ChatDeliveryState::Unconfirmed`] or [`ChatDeliveryState::NotObserved`]
/// for at least one addressed window -- all three mean the envelope was
/// written, and only the window's own reaction differs),
/// nobody has replied, `elapsed_ms` has reached `silence_timeout_ms`
/// (equality counts as elapsed), the room is not halted, and it has not
/// already been noted.
///
/// # Examples
///
/// ```
/// use yagura_daemon::room::{ChatDeliveryState, SilenceCheck, is_silence_candidate};
///
/// let check = SilenceCheck {
///     deliveries: vec![ChatDeliveryState::Confirmed],
///     replied_since: false,
///     elapsed_ms: 300_000,
///     halted: false,
///     silence_noted: false,
/// };
/// assert!(is_silence_candidate(&check, 300_000));
/// ```
#[must_use]
pub fn is_silence_candidate(check: &SilenceCheck, silence_timeout_ms: u64) -> bool {
    if check.halted || check.silence_noted || check.replied_since {
        return false;
    }
    let placed = check.deliveries.iter().any(|state| {
        matches!(
            state,
            ChatDeliveryState::Confirmed
                | ChatDeliveryState::Unconfirmed
                | ChatDeliveryState::NotObserved
        )
    });
    placed && check.elapsed_ms >= silence_timeout_ms
}

// =====================================================================
// The chat envelope
// =====================================================================

/// The fixed marker every chat envelope opens with.
const CHAT_ENVELOPE_PREFIX: &str = "[yagura chat] ";

/// Who a chat message is from, as carried in its envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEnvelopeFrom {
    /// An AI window.
    Window {
        /// The sending window's id.
        window_id: String,
        /// The sending window's display name.
        name: String,
    },
    /// The human user.
    User,
    /// A caller outside the daemon's window registry.
    External,
}

/// Why [`compose_chat_envelope`] refused to build an envelope: it would be
/// longer than the shared byte cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("chat envelope is {len} bytes (max {max})")]
pub struct ChatEnvelopeTooLong {
    /// The envelope's would-be length in bytes.
    pub len: usize,
    /// The limit that was exceeded ([`yagura_protocol::HANDOFF_ENVELOPE_MAX_BYTES`]).
    pub max: usize,
}

/// JSON-encodes `value` as a quoted string, escapes included.
///
/// Goes through [`serde_json::Value`]'s own `Display`, which cannot fail for
/// a string, rather than a fallible serializer whose error branch would have
/// no meaningful answer here. Kept local to this module (a near-identical
/// helper already exists in `handoff.rs`, but sharing it is outside this
/// module's writable files).
fn chat_json_string(value: &str) -> String {
    serde_json::Value::String(value.to_string()).to_string()
}

/// Composes a chat envelope: one line, the fixed prefix followed by a JSON
/// object with keys in a fixed order (`from`, `room_id`, `mentions`,
/// `broadcast`, `body`, `reply`). `mentions` carries resolved display names,
/// not window ids: the caller resolves names against the roster before
/// calling this. A `from` of [`ChatEnvelopeFrom::User`] or
/// [`ChatEnvelopeFrom::External`] serializes as the bare string `"user"` or
/// `"external"`; `reply` names `send_chat` and the room the message arrived
/// in, so a reader can answer into the same room without tracking its own
/// membership. Every
/// caller-supplied string goes in JSON-encoded, so text inside one can never
/// become structure.
///
/// # Errors
///
/// Returns [`ChatEnvelopeTooLong`] when the composed envelope is longer than
/// [`yagura_protocol::HANDOFF_ENVELOPE_MAX_BYTES`]. A length exactly at the cap is accepted.
///
/// # Examples
///
/// ```
/// use yagura_daemon::room::{ChatEnvelopeFrom, compose_chat_envelope};
///
/// let envelope = compose_chat_envelope(&ChatEnvelopeFrom::User, "main", &[], true, "hi")
///     .expect("a short envelope must fit under the byte cap");
/// assert!(envelope.starts_with("[yagura chat] "));
/// ```
pub fn compose_chat_envelope(
    from: &ChatEnvelopeFrom,
    room_id: &str,
    mentions: &[String],
    broadcast: bool,
    body: &str,
) -> Result<String, ChatEnvelopeTooLong> {
    let from_json = match from {
        ChatEnvelopeFrom::Window { window_id, name } => format!(
            "{{\"window_id\":{},\"name\":{}}}",
            chat_json_string(window_id),
            chat_json_string(name)
        ),
        ChatEnvelopeFrom::User => "\"user\"".to_string(),
        ChatEnvelopeFrom::External => "\"external\"".to_string(),
    };
    let mentions_json = {
        let encoded: Vec<String> = mentions.iter().map(|m| chat_json_string(m)).collect();
        format!("[{}]", encoded.join(","))
    };

    let room_id_json = chat_json_string(room_id);
    let text = format!(
        "{CHAT_ENVELOPE_PREFIX}{{\"from\":{from_json},\"room_id\":{room_id_json},\
         \"mentions\":{mentions_json},\"broadcast\":{broadcast},\"body\":{},\"reply\":\
         {{\"tool\":\"send_chat\",\"room_id\":{room_id_json}}}}}",
        chat_json_string(body)
    );

    // The cap and the comparison are `send_handoff`'s, not a second copy of
    // them: both envelopes are written into the same composer, so the length
    // a human is asked to read is one rule. The breakdown
    // `check_envelope_len` carries is a handoff's field split, which a chat
    // envelope has no fields for, so only `len` / `max` travel onward.
    let envelope = handoff::Envelope {
        parts: handoff::EnvelopeParts {
            overhead: text.len(),
            name: 0,
            cwd: 0,
            reference: 0,
            note: 0,
        },
        text,
    };
    match handoff::check_envelope_len(&envelope) {
        Ok(()) => Ok(envelope.text),
        Err(err) => Err(ChatEnvelopeTooLong {
            len: err.len,
            max: err.max,
        }),
    }
}

// =====================================================================
// `[chat]` limits' defaults and range
// =====================================================================

/// The `[chat]` policy limits: how many repeats, how long a monologue, how
/// big a turn, and how long a silence are tolerated before the room halts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatLimits {
    /// How many same-position messages within `repetition_window` trip the
    /// repetition halt.
    pub repetition_count: u64,
    /// How many trailing non-system messages the repetition check looks
    /// back over.
    pub repetition_window: u64,
    /// The maximum simhash distance at which two bodies still count as the
    /// same repeated message (0-64).
    pub repetition_distance: u64,
    /// How many consecutive same-speaker messages trip the monologue halt.
    pub monologue_max: u64,
    /// How many AI messages since the last user message trip the
    /// turn-budget halt.
    pub turn_message_max: u64,
    /// How long, in seconds, an unanswered message waits before it becomes
    /// a silence candidate (1-86400).
    pub silence_timeout_secs: u64,
    /// How many entries the room's history retains.
    pub history_max: u64,
}

impl Default for ChatLimits {
    fn default() -> Self {
        Self {
            repetition_count: 4,
            repetition_window: 12,
            repetition_distance: 6,
            monologue_max: 3,
            turn_message_max: 30,
            silence_timeout_secs: 300,
            history_max: 5_000,
        }
    }
}

/// Why a [`ChatLimits`] field was rejected: `value` falls outside
/// `min..=max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{field} must be between {min} and {max}, got {value}")]
pub struct ChatLimitsError {
    /// The rejected field's name.
    pub field: &'static str,
    /// The rejected value.
    pub value: u64,
    /// The field's minimum accepted value.
    pub min: u64,
    /// The field's maximum accepted value.
    pub max: u64,
}

/// Checks one field against `min..=max`, naming `field` in the rejection.
fn check_field_range(
    field: &'static str,
    value: u64,
    min: u64,
    max: u64,
) -> Result<(), ChatLimitsError> {
    if value >= min && value <= max {
        Ok(())
    } else {
        Err(ChatLimitsError {
            field,
            value,
            min,
            max,
        })
    }
}

impl ChatLimits {
    /// Validates every field against its documented range. This is the only
    /// place those ranges are checked: callers that load a `[chat]` policy
    /// from a playbook go through this same function rather than repeating
    /// the bounds.
    ///
    /// # Errors
    ///
    /// Returns the first out-of-range field found, checked in field
    /// declaration order.
    ///
    /// # Examples
    ///
    /// ```
    /// use yagura_daemon::room::ChatLimits;
    ///
    /// assert_eq!(ChatLimits::default().validate(), Ok(()));
    /// ```
    pub fn validate(&self) -> Result<(), ChatLimitsError> {
        check_field_range("repetition_count", self.repetition_count, 1, u64::MAX)?;
        check_field_range("repetition_window", self.repetition_window, 1, u64::MAX)?;
        check_field_range("repetition_distance", self.repetition_distance, 0, 64)?;
        check_field_range("monologue_max", self.monologue_max, 1, u64::MAX)?;
        check_field_range("turn_message_max", self.turn_message_max, 1, u64::MAX)?;
        check_field_range("silence_timeout_secs", self.silence_timeout_secs, 1, 86_400)?;
        check_field_range("history_max", self.history_max, 1, u64::MAX)?;
        Ok(())
    }
}
