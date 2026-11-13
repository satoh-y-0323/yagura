//! The pure functions behind `send_handoff` and `send_message`: composing the
//! envelopes put in a window's inbox, measuring them against the shared byte
//! cap, saying when a waiting one is handed over, and deciding which statuses
//! a handoff may land on.
//!
//! No I/O and no policy live here, so the envelope's exact bytes can be
//! checked without a window, a connection or a clock.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use yagura_protocol::{HANDOFF_ENVELOPE_MAX_BYTES, WindowStatus};

/// The fixed marker every envelope opens with, so a receiving CLI can tell a
/// handoff apart from something a human typed.
const ENVELOPE_PREFIX: &str = "[yagura handoff] ";

/// Who a handoff is from, as established by the requesting connection's
/// identity rather than by anything the caller sent.
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::EnvelopeFrom;
///
/// let requester = EnvelopeFrom::Window {
///     id: "w1".to_string(),
///     name: "claude-1".to_string(),
///     cwd: "/work/repo".to_string(),
/// };
/// assert_ne!(requester, EnvelopeFrom::External);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeFrom {
    /// A window the daemon still knows about, so the receiver can reply to it.
    Window {
        /// The requesting window's id.
        id: String,
        /// The requesting window's display name.
        name: String,
        /// The requesting window's working directory, the base a relative
        /// `reference` is resolved against.
        cwd: String,
    },
    /// A caller with no window of its own (or one the registry no longer
    /// holds), which therefore cannot be replied to.
    External,
}

/// A composed handoff envelope: the exact text to write, plus how its bytes
/// are accounted for.
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::{EnvelopeFrom, compose_envelope};
///
/// let envelope = compose_envelope(&EnvelopeFrom::External, "src/foo.rs", "please review");
/// assert!(envelope.text.starts_with("[yagura handoff] "));
/// assert!(!envelope.text.contains('\n'));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// The single line written into the receiving window's PTY.
    pub text: String,
    /// Where `text`'s bytes went.
    pub parts: EnvelopeParts,
}

/// Byte counts *after* JSON encoding. `name` / `cwd` / `reference` / `note`
/// are the encoded contents of those four string values (escapes included,
/// surrounding quotes excluded; `name` and `cwd` are 0 for an external
/// requester). `overhead` is everything else: the prefix, every byte of JSON
/// syntax, and the requester's window id wherever it appears. The five
/// fields always sum to `text.len()`.
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::{EnvelopeFrom, compose_envelope};
///
/// let envelope = compose_envelope(&EnvelopeFrom::External, "r", "t");
/// let parts = envelope.parts;
/// assert_eq!(parts.name, 0);
/// assert_eq!(parts.cwd, 0);
/// assert_eq!(
///     parts.overhead + parts.name + parts.cwd + parts.reference + parts.note,
///     envelope.text.len()
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvelopeParts {
    /// The prefix, the JSON syntax and the requester's window id.
    pub overhead: usize,
    /// The requester's display name, JSON-encoded.
    pub name: usize,
    /// The requester's working directory, JSON-encoded.
    pub cwd: usize,
    /// The handoff's `reference`, JSON-encoded.
    pub reference: usize,
    /// The handoff's `note`, JSON-encoded.
    pub note: usize,
}

/// Why an envelope was refused: it is longer than a human is asked to read
/// before approving it.
///
/// The breakdown travels with the rejection so a caller is told which field
/// to shorten instead of having to guess.
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::{EnvelopeFrom, check_envelope_len, compose_envelope};
/// use yagura_protocol::HANDOFF_ENVELOPE_MAX_BYTES;
///
/// let long_note = "x".repeat(HANDOFF_ENVELOPE_MAX_BYTES);
/// let envelope = compose_envelope(&EnvelopeFrom::External, "r", &long_note);
/// let err = check_envelope_len(&envelope).expect_err("this envelope is over the cap");
/// assert_eq!(err.max, HANDOFF_ENVELOPE_MAX_BYTES);
/// assert!(err.to_string().contains("(JSON-encoded sizes)"));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "envelope is {len} bytes (max {max}): overhead {}, name {}, cwd {}, reference {}, note {} (JSON-encoded sizes)",
    parts.overhead,
    parts.name,
    parts.cwd,
    parts.reference,
    parts.note
)]
pub struct EnvelopeTooLong {
    /// The envelope's actual length in bytes.
    pub len: usize,
    /// The limit that was exceeded ([`HANDOFF_ENVELOPE_MAX_BYTES`]).
    pub max: usize,
    /// How those bytes were spent.
    pub parts: EnvelopeParts,
}

/// Composes the envelope for a handoff from `from`, carrying `reference` and
/// `note`.
///
/// The result is one line: the fixed prefix followed by a JSON object whose
/// keys appear in a fixed order (`from`, `reference`, `note`, `reply`), which
/// is why the object is assembled from encoded values rather than through a
/// JSON map (a map orders its keys for its own purposes, not the reader's).
/// Every caller-supplied value goes in as a JSON string, so text inside one
/// can never become structure: a `note` that spells out a different reply
/// target stays a note.
///
/// An [`EnvelopeFrom::External`] requester cannot be replied to, so its
/// envelope carries `"from":"external"` and no `reply` object at all.
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::{EnvelopeFrom, compose_envelope};
///
/// let from = EnvelopeFrom::Window {
///     id: "A".to_string(),
///     name: "n".to_string(),
///     cwd: "c".to_string(),
/// };
/// let envelope = compose_envelope(&from, "r", "t");
/// assert_eq!(
///     envelope.text,
///     "[yagura handoff] {\"from\":{\"window_id\":\"A\",\"name\":\"n\",\"cwd\":\"c\"},\
///      \"reference\":\"r\",\"note\":\"t\",\"reply\":{\"tool\":\"send_handoff\",\"window_id\":\"A\"}}"
/// );
/// assert_eq!(envelope.parts.note, 1);
/// ```
#[must_use]
pub fn compose_envelope(from: &EnvelopeFrom, reference: &str, note: &str) -> Envelope {
    let encoded_reference = json_string(reference);
    let encoded_note = json_string(note);

    let (from_json, reply_json, encoded_name, encoded_cwd) = match from {
        EnvelopeFrom::Window { id, name, cwd } => {
            let encoded_id = json_string(id);
            let encoded_name = json_string(name);
            let encoded_cwd = json_string(cwd);
            let from_json = format!(
                "{{\"window_id\":{encoded_id},\"name\":{encoded_name},\"cwd\":{encoded_cwd}}}"
            );
            let reply_json =
                format!(",\"reply\":{{\"tool\":\"send_handoff\",\"window_id\":{encoded_id}}}");
            (from_json, reply_json, encoded_name, encoded_cwd)
        }
        EnvelopeFrom::External => (
            "\"external\"".to_string(),
            String::new(),
            String::new(),
            String::new(),
        ),
    };

    let text = format!(
        "{ENVELOPE_PREFIX}{{\"from\":{from_json},\"reference\":{encoded_reference},\
         \"note\":{encoded_note}{reply_json}}}"
    );

    let name = content_len(&encoded_name);
    let cwd = content_len(&encoded_cwd);
    let reference = content_len(&encoded_reference);
    let note = content_len(&encoded_note);
    let parts = EnvelopeParts {
        overhead: text.len() - name - cwd - reference - note,
        name,
        cwd,
        reference,
        note,
    };

    Envelope { text, parts }
}

/// Checks a composed envelope against [`HANDOFF_ENVELOPE_MAX_BYTES`].
///
/// # Errors
///
/// Returns [`EnvelopeTooLong`], carrying the breakdown, when the envelope is
/// longer than the cap. A length exactly at the cap is accepted.
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::{EnvelopeFrom, check_envelope_len, compose_envelope};
///
/// let envelope = compose_envelope(&EnvelopeFrom::External, "src/foo.rs", "please review");
/// assert_eq!(check_envelope_len(&envelope), Ok(()));
/// ```
pub fn check_envelope_len(envelope: &Envelope) -> Result<(), EnvelopeTooLong> {
    let len = envelope.text.len();
    if len <= HANDOFF_ENVELOPE_MAX_BYTES {
        return Ok(());
    }
    Err(EnvelopeTooLong {
        len,
        max: HANDOFF_ENVELOPE_MAX_BYTES,
        parts: envelope.parts,
    })
}

/// How many random bytes name one envelope.
const ENVELOPE_ID_RANDOM_BYTES: usize = 12;

/// Names an envelope when the OS RNG cannot.
static ENVELOPE_ID_FALLBACK: AtomicU64 = AtomicU64::new(0);

/// A new id for one envelope a sender sends (`send_message`, `send_handoff`):
/// a receiver that is handed the same id twice knows it is one request.
///
/// A failure to obtain randomness falls back to a counter rather than failing
/// the send: the counter still keeps every id this process draws distinct.
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::new_envelope_id;
///
/// assert_ne!(new_envelope_id(), new_envelope_id());
/// ```
#[must_use]
pub fn new_envelope_id() -> String {
    let mut bytes = [0u8; ENVELOPE_ID_RANDOM_BYTES];
    match yagura_os::fill_random(&mut bytes) {
        Ok(()) => bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
        Err(err) => {
            tracing::warn!("failed to obtain randomness for an envelope id ({err})");
            format!(
                "fallback-{}",
                ENVELOPE_ID_FALLBACK.fetch_add(1, Ordering::Relaxed)
            )
        }
    }
}

/// `text`, an envelope (a fixed prefix followed by one JSON object), with
/// `"id"` as its object's first key. A text with no object is returned as
/// it is.
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::with_envelope_id;
///
/// assert_eq!(
///     with_envelope_id("[yagura chat] {\"body\":\"hi\"}", "chat:main:3"),
///     "[yagura chat] {\"id\":\"chat:main:3\",\"body\":\"hi\"}"
/// );
/// ```
#[must_use]
pub fn with_envelope_id(text: &str, id: &str) -> String {
    match text.split_once('{') {
        Some((prefix, rest)) => format!("{prefix}{{\"id\":{},{rest}", json_string(id)),
        None => text.to_string(),
    }
}

/// The id an envelope this module composed carries (its object's `"id"`);
/// `None` for a text that carries none.
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::{EnvelopeFrom, compose_message_envelope, envelope_id};
///
/// let envelope = compose_message_envelope(&EnvelopeFrom::External, "m1", "hi");
/// assert_eq!(envelope_id(&envelope).as_deref(), Some("m1"));
/// assert_eq!(envelope_id("no object"), None);
/// ```
#[must_use]
pub fn envelope_id(text: &str) -> Option<String> {
    let (_, object) = text.split_once('{')?;
    let object: Value = serde_json::from_str(&format!("{{{object}")).ok()?;
    object.get("id")?.as_str().map(str::to_string)
}

/// [`compose_envelope`] with `id` as the object's first key, counted in
/// [`EnvelopeParts::overhead`]: the envelope a `send_handoff` puts in a
/// window's inbox or starts a window with.
#[must_use]
pub fn compose_identified_envelope(
    from: &EnvelopeFrom,
    id: &str,
    reference: &str,
    note: &str,
) -> Envelope {
    let envelope = compose_envelope(from, reference, note);
    let text = with_envelope_id(&envelope.text, id);
    let parts = EnvelopeParts {
        overhead: envelope.parts.overhead + text.len() - envelope.text.len(),
        ..envelope.parts
    };
    Envelope { text, parts }
}

/// The fixed marker every message envelope opens with.
const MESSAGE_ENVELOPE_PREFIX: &str = "[yagura message] ";

/// Composes the envelope `send_message` puts in a window's inbox: one line,
/// the fixed prefix followed by a JSON object with the keys `id`, `from`,
/// `text` and, when `from` can be answered, `reply`. `text` goes in as a JSON
/// string, so nothing inside it can become structure.
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::{EnvelopeFrom, compose_message_envelope};
///
/// assert_eq!(
///     compose_message_envelope(&EnvelopeFrom::External, "m1", "two\nlines"),
///     "[yagura message] {\"id\":\"m1\",\"from\":\"external\",\"text\":\"two\\nlines\"}"
/// );
/// ```
#[must_use]
pub fn compose_message_envelope(from: &EnvelopeFrom, id: &str, text: &str) -> String {
    let (from_json, reply_json) = match from {
        EnvelopeFrom::Window { id, name, cwd } => {
            let encoded_id = json_string(id);
            (
                format!(
                    "{{\"window_id\":{encoded_id},\"name\":{},\"cwd\":{}}}",
                    json_string(name),
                    json_string(cwd)
                ),
                format!(",\"reply\":{{\"tool\":\"send_message\",\"window_id\":{encoded_id}}}"),
            )
        }
        EnvelopeFrom::External => ("\"external\"".to_string(), String::new()),
    };
    format!(
        "{MESSAGE_ENVELOPE_PREFIX}{{\"id\":{},\"from\":{from_json},\"text\":{}{reply_json}}}",
        json_string(id),
        json_string(text)
    )
}

/// How a window's profile lets the daemon hand its CLI what waits for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitingWake {
    /// The CLI leaves a wait registered that the daemon answers.
    Wait,
    /// The daemon runs the CLI's own command.
    Command,
    /// Nothing wakes the CLI: it reads at its own doors only.
    Doors,
    /// The window declares no delivery at all.
    Undeclared,
}

/// The facts [`waiting_note`] says when a waiting envelope is handed over
/// from: the window's profile and its state now, never the name of its CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitingFacts {
    /// How the window's CLI is handed what waits.
    pub wake: WaitingWake,
    /// The window's status now.
    pub status: WindowStatus,
    /// Whether the CLI has a wait registered now.
    pub has_waiter: bool,
    /// Whether the daemon asks a CLI with no wait registered to register one
    /// again: the window's open named a wake file.
    pub asks_again: bool,
    /// Whether the CLI has reported a session the wake command can name.
    pub session_known: bool,
    /// Whether a door is carrying the envelope and has not said it printed it.
    pub carried: bool,
    /// Whether the envelope outlives a daemon restart.
    pub saved: bool,
}

/// Says when an envelope that is not delivered yet is handed over, and what
/// could lose it on the way. `None` when there is nothing to say: the CLI has
/// a wait registered that the daemon answers. Never says the CLI can or
/// cannot be woken: whether it was is what the answer's state says.
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::{WaitingFacts, WaitingWake, waiting_note};
/// use yagura_protocol::WindowStatus;
///
/// let facts = WaitingFacts {
///     wake: WaitingWake::Wait,
///     status: WindowStatus::Running,
///     has_waiter: false,
///     asks_again: false,
///     session_known: true,
///     carried: false,
///     saved: false,
/// };
/// assert!(waiting_note(facts).is_some());
/// assert_eq!(waiting_note(WaitingFacts { has_waiter: true, ..facts }), None);
/// ```
#[must_use]
pub fn waiting_note(facts: WaitingFacts) -> Option<String> {
    let when = if facts.carried {
        "A door of the window's CLI is carrying it and has not said it printed it yet."
    } else if facts.status == WindowStatus::Unknown {
        "The window's status is unknown, so the daemon pushes nothing to it; it is handed \
         over at the CLI's next door, or pushed once the status is known again."
    } else if facts.status == WindowStatus::WaitingApproval {
        "The window waits for an approval; it is handed over at the CLI's next door after \
         that."
    } else {
        match facts.wake {
            WaitingWake::Wait if facts.has_waiter => return None,
            WaitingWake::Wait if facts.asks_again => {
                "No wait of the window's CLI was registered, so the daemon asked the CLI to \
                 register one again; it is handed over once the CLI does, or at its next turn."
            }
            WaitingWake::Wait => {
                "No wait of the window's CLI is registered right now; it arrives at the CLI's \
                 next turn."
            }
            WaitingWake::Command if !facts.session_known => {
                "The window's CLI has not reported a session yet; it arrives at the CLI's \
                 first turn."
            }
            WaitingWake::Command => "It arrives at the CLI's next turn.",
            WaitingWake::Doors => {
                "Nothing wakes the window's CLI; it arrives at the CLI's next turn, such as \
                 when the user next sends a prompt in this window."
            }
            WaitingWake::Undeclared => {
                "The window declares no delivery; its CLI sees it only when it reads it \
                 itself."
            }
        }
    };
    Some(if facts.saved {
        when.to_string()
    } else {
        format!("{when} It is lost if the daemon restarts before then.")
    })
}

/// The statuses a window may receive a handoff in: it is waiting for input,
/// so a line written now is read as one message rather than landing in the
/// middle of whatever it was doing.
///
/// `Unknown` is deliberately absent, and stays absent through
/// [`crate::waiter::wait_for_any`], which never settles on it.
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::READY_STATUSES;
/// use yagura_protocol::WindowStatus;
///
/// assert!(READY_STATUSES.contains(&WindowStatus::Idle));
/// assert!(!READY_STATUSES.contains(&WindowStatus::Unknown));
/// ```
pub const READY_STATUSES: &[WindowStatus] = &[WindowStatus::WaitingInput, WindowStatus::Idle];

/// Whether `status` is one of [`READY_STATUSES`].
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::is_ready;
/// use yagura_protocol::WindowStatus;
///
/// assert!(is_ready(WindowStatus::WaitingInput));
/// assert!(!is_ready(WindowStatus::Running));
/// ```
#[must_use]
pub fn is_ready(status: WindowStatus) -> bool {
    READY_STATUSES.contains(&status)
}

/// The fixed marker every task notice starts with.
const TASK_NOTICE_MARKER: &str = "[yagura task]";

/// Composes the one-line `[yagura task]` notice `open_task_window` writes
/// into a task's window once it accepts input: names `plan_id` and
/// `task_id` and tells the receiver to read the assigned request through
/// `get_plan`. Never takes the request's own text, so the body a task
/// carries cannot reach this line by construction.
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::compose_task_notice;
///
/// let notice = compose_task_notice("plan-1", "build-parser");
/// assert!(notice.starts_with("[yagura task] "));
/// assert!(notice.contains("plan-1"));
/// assert!(notice.contains("build-parser"));
/// ```
#[must_use]
pub fn compose_task_notice(plan_id: &str, task_id: &str) -> String {
    format!(
        "{TASK_NOTICE_MARKER} Task {task_id} of plan {plan_id} is assigned to this window. \
         Call get_plan with plan_id \"{plan_id}\" and task_id \"{task_id}\" to read its \
         instruction."
    )
}

/// The fixed marker every plan-approval notice starts with.
const PLAN_NOTICE_MARKER: &str = "[yagura plan]";

/// Composes the one-line `[yagura plan]` notice queued on a plan's owner
/// once an approver settles it: names `plan_id` only, never the plan's
/// title -- the same reason [`compose_task_notice`] never carries a task's
/// own request text, a title is free-form text a later owner (after
/// `set_plan_owner`) did not write.
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::compose_plan_approved_notice;
///
/// let notice = compose_plan_approved_notice("plan-1");
/// assert!(notice.starts_with("[yagura plan] "));
/// assert!(notice.contains("plan-1"));
/// ```
#[must_use]
pub fn compose_plan_approved_notice(plan_id: &str) -> String {
    format!("{PLAN_NOTICE_MARKER} Plan {plan_id} was approved. You can open its tasks now.")
}

/// JSON-encodes `value` as a quoted string, escapes included.
///
/// Goes through [`Value`]'s own `Display`, which cannot fail for a string,
/// rather than through a fallible serializer whose error branch would have no
/// meaningful answer here.
fn json_string(value: &str) -> String {
    Value::String(value.to_string()).to_string()
}

/// The content length of an encoded JSON string: its bytes without the
/// surrounding quotes. An empty input stands for an absent field and measures
/// 0 rather than underflowing.
fn content_len(encoded: &str) -> usize {
    encoded.len().saturating_sub(2)
}
