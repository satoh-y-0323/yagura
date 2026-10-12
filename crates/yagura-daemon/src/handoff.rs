//! The pure functions behind `send_handoff`: composing the envelope written
//! into the receiving window's PTY, measuring it against the shared byte cap,
//! and deciding which statuses a handoff may land on.
//!
//! No I/O and no policy live here, so the envelope's exact bytes can be
//! checked without a window, a connection or a clock.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::time::Instant;
use yagura_protocol::{HANDOFF_ENVELOPE_MAX_BYTES, WindowStatus};

use crate::waiter::StatusAt;

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

/// The statuses that count as a reaction to a handoff that was written and
/// submitted: the receiver stopped accepting input and started doing
/// something with what it was given.
///
/// This is the complement of [`READY_STATUSES`] minus `Unknown` (a window
/// nothing can be told about neither confirms nor refutes a delivery) and
/// `Exited` (a different ending, not a reaction).
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::DELIVERY_REACTION_STATUSES;
/// use yagura_protocol::WindowStatus;
///
/// assert!(DELIVERY_REACTION_STATUSES.contains(&WindowStatus::Running));
/// assert!(!DELIVERY_REACTION_STATUSES.contains(&WindowStatus::Exited));
/// ```
pub const DELIVERY_REACTION_STATUSES: &[WindowStatus] = &[
    WindowStatus::Running,
    WindowStatus::WaitingApproval,
    WindowStatus::Interrupted,
];

/// Whether `status` is one of [`DELIVERY_REACTION_STATUSES`].
///
/// Every status is named, with no `_` arm, so an eighth status added later
/// has to be classified here rather than silently counting as no reaction.
///
/// # Examples
///
/// ```
/// use yagura_daemon::handoff::{is_ready, reacts_to_delivery};
/// use yagura_protocol::WindowStatus;
///
/// assert!(reacts_to_delivery(WindowStatus::Running));
/// assert!(!reacts_to_delivery(WindowStatus::Unknown));
/// // Ready and reacting are disjoint: a status is at most one of them.
/// assert!(!(is_ready(WindowStatus::Idle) && reacts_to_delivery(WindowStatus::Idle)));
/// ```
#[must_use]
pub fn reacts_to_delivery(status: WindowStatus) -> bool {
    match status {
        WindowStatus::Running | WindowStatus::WaitingApproval | WindowStatus::Interrupted => true,
        WindowStatus::WaitingInput
        | WindowStatus::Idle
        | WindowStatus::Exited
        | WindowStatus::Unknown => false,
    }
}

/// What a window's published status pair says about its startup phase.
///
/// `Left` means the window has once held a status that accepts input for the
/// whole hold, so nothing about its startup phase is pending any more.
/// `Building` means the window is in such a status right now but has not held
/// it long enough yet, so a caller may wait for the hold to complete.
/// `Absent` means the window's confirmed status is known -- it simply is not
/// the kind of status this evidence accumulates from, which is why a caller
/// may not read `Absent` as "undetectable" (`Unknown` is refused by the
/// status gate before it ever reaches here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupEvidence {
    /// The window has left its startup phase.
    Left,
    /// The window is accumulating the hold right now.
    Building,
    /// The current status contributes nothing towards the hold.
    Absent,
}

/// One window's record of whether it has left its startup phase: a prompt a
/// CLI paints while it is still starting up is indistinguishable from one it
/// paints when it is ready, so the daemon asks the prompt to *stay*.
///
/// Monotone: a window that has once held a status that accepts input for
/// `hold` has left its startup phase, and nothing it does afterwards takes
/// that back.
///
/// What the latch is read for is that a caller may skip the wait -- never
/// that a caller must refuse to write.
///
/// A zero `hold` is latched from construction, which is what makes a
/// definition without `ready_hold_ms` behave exactly as it did before this
/// existed.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use tokio::time::Instant;
/// use yagura_daemon::handoff::{ReadyHoldLatch, StartupEvidence};
/// use yagura_daemon::waiter::StatusAt;
/// use yagura_protocol::DetectedStatus;
///
/// let latch = ReadyHoldLatch::new(Duration::ZERO);
/// let now = Instant::now();
/// let observed = StatusAt {
///     status: DetectedStatus::Running,
///     changed_at: now,
///     evidence_since: now,
///     generation: 1,
/// };
/// assert_eq!(latch.observe(observed, now), StartupEvidence::Left);
/// ```
#[derive(Debug)]
pub struct ReadyHoldLatch {
    hold: Duration,
    latched: AtomicBool,
}

impl ReadyHoldLatch {
    /// A latch that asks a status accepting input to hold for `hold`.
    #[must_use]
    pub fn new(hold: Duration) -> Self {
        Self {
            hold,
            latched: AtomicBool::new(hold == Duration::ZERO),
        }
    }

    /// Whether this window has already been shown to be past its startup
    /// phase.
    #[must_use]
    pub fn is_latched(&self) -> bool {
        self.latched.load(Ordering::SeqCst)
    }

    /// Evaluates one observed pair, latching when it proves the hold was
    /// held.
    ///
    /// The caller hands in the pair it observed and the instant it observed
    /// it, so this holds no clock of its own.
    pub fn observe(&self, observed: StatusAt, now: Instant) -> StartupEvidence {
        if self.is_latched() {
            return StartupEvidence::Left;
        }
        if !is_ready(observed.status.status()) {
            return StartupEvidence::Absent;
        }
        if now.saturating_duration_since(observed.changed_at) >= self.hold {
            self.latch();
            return StartupEvidence::Left;
        }
        StartupEvidence::Building
    }

    /// Records that this window is past its startup phase, for a caller that
    /// waited the hold out itself -- the wait is the proof.
    pub fn latch(&self) {
        self.latched.store(true, Ordering::SeqCst);
    }
}

/// Whether a `hold` can ever be satisfied inside a readiness wait bounded by
/// `timeout`. Equality is *not* reachable: the wait ends at the deadline, so
/// a hold that completes exactly then never settles it.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use yagura_daemon::handoff::hold_is_reachable_within;
///
/// assert!(hold_is_reachable_within(
///     Duration::from_millis(1_000),
///     Duration::from_millis(2_000)
/// ));
/// assert!(!hold_is_reachable_within(
///     Duration::from_millis(2_000),
///     Duration::from_millis(2_000)
/// ));
/// ```
#[must_use]
pub fn hold_is_reachable_within(hold: Duration, timeout: Duration) -> bool {
    hold < timeout
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
