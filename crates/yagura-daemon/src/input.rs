//! How one line of text is handed to a particular CLI's input: the profile a
//! detection definition's `[input]` table resolves to, and the ordered write
//! sequence that submits a body through it.
//!
//! Everything here is pure. No PTY, no window, no clock and no policy: the
//! bytes that would be written, and the waiting they carry, can be checked
//! without a child process. Executing a plan, and deciding whether a window
//! may be written to at all, belong to the layers above.

use std::time::Duration;

use tokio::time::Instant;
use yagura_protocol::WindowStatus;

use crate::detector::definition::{INPUT_SUBMIT_DELAY_DEFAULT_MS, InputSection};

/// The escape byte a body must not contain for the daemon to frame it: a body
/// carrying `ESC` could spell the end marker itself and leave the frame.
const ESC: u8 = 0x1b;

/// Opens a bracketed-paste sequence: the receiving CLI takes what follows as
/// pasted text rather than as keystrokes.
///
/// # Examples
///
/// ```
/// use yagura_daemon::input::BRACKETED_PASTE_START;
///
/// assert_eq!(BRACKETED_PASTE_START, b"\x1b[200~");
/// ```
pub const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";

/// Closes a bracketed-paste sequence opened by [`BRACKETED_PASTE_START`].
///
/// # Examples
///
/// ```
/// use yagura_daemon::input::BRACKETED_PASTE_END;
///
/// assert_eq!(BRACKETED_PASTE_END, b"\x1b[201~");
/// ```
pub const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

/// How one line of text is handed to a particular CLI's input and submitted.
///
/// The values come from a detection definition's `[input]` table, with this
/// module's defaults applied for whatever the definition omits: the loader
/// stores raw `Option`s so "not written" stays distinguishable from "written
/// as the default", and the defaults live here, at their single application
/// point.
///
/// # Examples
///
/// ```
/// use yagura_daemon::input::InputProfile;
///
/// let profile = InputProfile::default();
/// assert_eq!(profile.submit, b"\r");
/// assert!(profile.clear.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputProfile {
    /// Whether the body is wrapped in [`BRACKETED_PASTE_START`] /
    /// [`BRACKETED_PASTE_END`] before being written.
    pub bracketed_paste: bool,
    /// The bytes that submit a line, written verbatim after the body.
    pub submit: Vec<u8>,
    /// One key that takes text back out of the receiver's editor, safe to
    /// repeat: on an empty editor it does nothing, so a take-back writes it
    /// once per display line the body may occupy (see
    /// [`InputProfile::plan_take_back`]). Empty means "send nothing", so
    /// nothing can be taken back.
    pub clear: Vec<u8>,
    /// How long a status that accepts input must have held, once, for this
    /// window to count as past its startup phase. `Duration::ZERO` counts it
    /// as past immediately.
    pub ready_hold: Duration,
    /// How long to wait after writing the body before writing `submit`.
    pub submit_delay: Duration,
    /// How long to wait for a delivery reaction after `submit` was written.
    /// `Duration::ZERO` disables the wait.
    pub delivery_confirm: Duration,
    /// How many times a submit may be re-sent after an unconfirmed delivery.
    pub resend_max: u8,
    /// One printable character the receiver's editor shows as typed, written
    /// behind the daemon's own writes: the receiver handles input in order, so
    /// once it shows, everything written before it has been handled. `None`
    /// means the daemon writes no judged line into the window.
    pub probe: Option<char>,
    /// The key that erases one character, used to take the probe back out.
    pub erase: Vec<u8>,
}

impl Default for InputProfile {
    /// The daemon's defaults, chosen on the safe side: no paste framing for a
    /// receiver of unknown make, a submit delay long enough for a large body
    /// to settle, and no re-sending at all until a definition asks for it.
    fn default() -> Self {
        Self {
            bracketed_paste: false,
            submit: b"\r".to_vec(),
            clear: Vec::new(),
            ready_hold: Duration::ZERO,
            submit_delay: Duration::from_millis(INPUT_SUBMIT_DELAY_DEFAULT_MS),
            delivery_confirm: Duration::from_millis(8_000),
            resend_max: 0,
            probe: None,
            erase: b"\x7f".to_vec(),
        }
    }
}

impl InputProfile {
    /// Applies the daemon's defaults to a definition's `[input]` table
    /// (`None` = the definition omitted it, or none loaded).
    ///
    /// The section's values are taken as loaded; range checking happened when
    /// the definition was parsed, so nothing here can fail.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    /// use yagura_daemon::input::InputProfile;
    ///
    /// let profile = InputProfile::from_section(None);
    /// assert!(!profile.bracketed_paste);
    /// assert_eq!(profile.submit_delay, Duration::from_millis(800));
    /// assert_eq!(profile.resend_max, 0);
    /// ```
    #[must_use]
    pub fn from_section(section: Option<&InputSection>) -> Self {
        let defaults = Self::default();
        let Some(section) = section else {
            return defaults;
        };
        Self {
            bracketed_paste: section.bracketed_paste.unwrap_or(defaults.bracketed_paste),
            submit: section
                .submit
                .as_ref()
                .map_or(defaults.submit, |value| value.as_bytes().to_vec()),
            clear: section
                .clear
                .as_ref()
                .map_or(defaults.clear, |value| value.as_bytes().to_vec()),
            ready_hold: section
                .ready_hold_ms
                .map_or(defaults.ready_hold, Duration::from_millis),
            submit_delay: section
                .submit_delay_ms
                .map_or(defaults.submit_delay, Duration::from_millis),
            delivery_confirm: section
                .delivery_confirm_ms
                .map_or(defaults.delivery_confirm, Duration::from_millis),
            resend_max: section.resend_max.unwrap_or(defaults.resend_max),
            probe: section
                .probe
                .as_deref()
                .and_then(|value| value.chars().next()),
            erase: section
                .erase
                .as_ref()
                .map_or(defaults.erase, |value| value.as_bytes().to_vec()),
        }
    }

    /// Whether this profile may re-submit at all.
    ///
    /// A resend that finds its own envelope still on screen (or vouched for
    /// by the write record) re-sends the submit key alone, so it needs no
    /// clear sequence of its own; a definition with no
    /// `clear` may therefore still resend.
    ///
    /// # Examples
    ///
    /// ```
    /// use yagura_daemon::input::InputProfile;
    ///
    /// let mut profile = InputProfile::default();
    /// assert!(!profile.may_resend(), "resend_max defaults to 0");
    /// profile.resend_max = 2;
    /// assert!(profile.may_resend());
    /// ```
    #[must_use]
    pub fn may_resend(&self) -> bool {
        self.resend_max > 0
    }

    /// How this profile would take a written body back out of the receiver's
    /// input.
    ///
    /// A non-empty `clear` takes back any body, however long, by being
    /// repeated (see [`InputProfile::plan_take_back`]); without one the body
    /// cannot be taken back.
    ///
    /// # Examples
    ///
    /// ```
    /// use yagura_daemon::input::{InputProfile, TakeBack};
    ///
    /// let mut profile = InputProfile::default();
    /// assert_eq!(profile.take_back(), TakeBack::Unavailable);
    ///
    /// profile.clear = b"\x15".to_vec();
    /// assert_eq!(profile.take_back(), TakeBack::Fixed);
    /// ```
    #[must_use]
    pub fn take_back(&self) -> TakeBack {
        if self.clear.is_empty() {
            TakeBack::Unavailable
        } else {
            TakeBack::Fixed
        }
    }

    /// The take-back for `body` in a window `cols` wide: the `clear` key
    /// repeated once per display line the body may occupy, plus a margin,
    /// all in one write and no submit key. No step at all when the profile
    /// cannot take a body back.
    ///
    /// The receiver handles input in order, so however late a stalled
    /// receiver gets to the body, enough keys follow it to empty what it
    /// painted; the keys beyond that land on an empty editor.
    ///
    /// # Examples
    ///
    /// ```
    /// use yagura_daemon::input::InputProfile;
    ///
    /// let mut profile = InputProfile::default();
    /// assert!(profile.plan_take_back("hello", 80).steps.is_empty());
    ///
    /// profile.clear = b"\x15".to_vec();
    /// let short = profile.plan_take_back("hello", 80);
    /// let long = profile.plan_take_back(&"x".repeat(1000), 80);
    /// assert_eq!(short.steps.len(), 1);
    /// assert!(long.steps[0].bytes.len() > short.steps[0].bytes.len());
    /// ```
    #[must_use]
    pub fn plan_take_back(&self, body: &str, cols: u16) -> SubmitPlan {
        let key = self.take_back_bytes();
        if key.is_empty() {
            return SubmitPlan { steps: Vec::new() };
        }
        let keys = take_back_key_count(body, cols);
        SubmitPlan {
            steps: vec![SubmitStep {
                bytes: key.repeat(keys),
                delay_after: Duration::ZERO,
            }],
        }
    }

    /// The bytes of one take-back key, empty when it cannot be taken back.
    fn take_back_bytes(&self) -> Vec<u8> {
        match self.take_back() {
            TakeBack::Unavailable => Vec::new(),
            TakeBack::Fixed => self.clear.clone(),
        }
    }
}

/// Columns of each display line a composer spends on its marker, separator
/// and continuation indent, so fewer remain for the body. An upper bound over
/// the shipped definitions, not a per-definition value: overestimating only
/// adds keys that land on an empty editor.
const COMPOSER_LINE_OVERHEAD_COLS: usize = 8;

/// How many take-back keys a `body` needs in a window `cols` wide: the display
/// lines it may occupy, doubled for CLIs that wrap at word boundaries and so
/// waste columns, plus a margin. Every character outside ASCII counts as two
/// columns, the widest a terminal cell gets, so wide text is never undercounted.
pub(crate) fn take_back_key_count(body: &str, cols: u16) -> usize {
    let usable = usize::from(cols)
        .saturating_sub(COMPOSER_LINE_OVERHEAD_COLS)
        .max(1);
    let lines = body.split('\n').fold(0usize, |lines, line| {
        let width = line.chars().fold(0usize, |width, c| {
            width.saturating_add(if c.is_ascii() { 1 } else { 2 })
        });
        lines.saturating_add(width.div_ceil(usable).max(1))
    });
    lines.saturating_mul(2).saturating_add(2)
}

/// How a profile would take a written body back (see
/// [`InputProfile::take_back`]).
///
/// # Examples
///
/// ```
/// use yagura_daemon::input::{InputProfile, TakeBack};
///
/// let mut profile = InputProfile::default();
/// assert_eq!(profile.take_back(), TakeBack::Unavailable);
///
/// profile.clear = b"\x15".to_vec();
/// assert_eq!(profile.take_back(), TakeBack::Fixed);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeBack {
    /// The body cannot be taken back: the profile defines no `clear`
    /// sequence.
    Unavailable,
    /// The profile's `clear` key, repeated enough times to empty any body of
    /// that length (see [`InputProfile::plan_take_back`]).
    Fixed,
}

/// One write plus how long to wait before the next one.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use yagura_daemon::input::{ComposerLine, InputProfile, SubmitAction, plan_submit_line};
///
/// let line = ComposerLine::new("hello");
/// let plan = plan_submit_line(&InputProfile::default(), &line, SubmitAction::Write);
/// let last = plan.steps.last().expect("a plan always ends with its submit key");
/// assert_eq!(last.delay_after, Duration::ZERO);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitStep {
    /// The bytes to write.
    pub bytes: Vec<u8>,
    /// How long to wait after this write completes, before the next step.
    /// `Duration::ZERO` on the final step, which has nothing to settle for.
    pub delay_after: Duration,
}

/// An ordered, atomic sequence of writes into one window's input.
///
/// # Examples
///
/// ```
/// use yagura_daemon::input::{ComposerLine, InputProfile, SubmitAction, plan_submit_line};
///
/// let line = ComposerLine::new("hello");
/// let plan = plan_submit_line(&InputProfile::default(), &line, SubmitAction::Write);
/// assert_eq!(plan.steps.len(), 2, "body then submit key");
/// assert_eq!(plan.steps[0].bytes, b"hello ");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitPlan {
    /// The writes, in the order they must be performed.
    pub steps: Vec<SubmitStep>,
}

/// What executing a [`SubmitPlan`] observed from inside the sequence.
///
/// Both fields are read inside the plan, under the window's input lock: read
/// from outside, the window is open between the body write and the read.
///
/// # Examples
///
/// ```
/// use tokio::time::Instant;
/// use yagura_daemon::input::SubmitOutcome;
/// use yagura_protocol::WindowStatus;
///
/// let outcome = SubmitOutcome {
///     status_before_submit: WindowStatus::WaitingInput,
///     submitted_at: Instant::now(),
/// };
/// assert_eq!(outcome.status_before_submit, WindowStatus::WaitingInput);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmitOutcome {
    /// The window's confirmed status immediately before the submit key was
    /// written. When this is not a status that accepts input, nothing
    /// observed afterwards can be told apart from the body's own echo.
    pub status_before_submit: WindowStatus,
    /// When the submit key's write completed: the start of the window in
    /// which a reaction counts as delivery. On the clock
    /// [`crate::waiter::StatusAt`] carries, which it is compared with.
    pub submitted_at: Instant,
}

/// The text the daemon writes into a window's composer for one message: the
/// body followed by one ASCII space, added whatever the body ends with.
///
/// A receiver's input completion opens on the word right before the cursor,
/// so the cursor must never rest on the end of the sender's own text.
///
/// # Examples
///
/// ```
/// use yagura_daemon::input::ComposerLine;
///
/// assert_eq!(ComposerLine::new("review @src").as_str(), "review @src ");
/// assert_eq!(ComposerLine::new("abc ").as_str(), "abc  ");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerLine(String);

impl ComposerLine {
    /// Builds the line for `body`, the text the caller wants to send.
    #[must_use]
    pub fn new(body: &str) -> Self {
        let mut line = String::with_capacity(body.len() + 1);
        line.push_str(body);
        line.push(' ');
        Self(line)
    }

    /// The text written into the composer, trailing space included.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The composer's text once `probe` is typed behind this line.
    #[must_use]
    pub fn with_probe(&self, probe: char) -> String {
        format!("{}{probe}", self.0)
    }
}

/// The write sequence [`plan_submit_line`] builds for one submit attempt.
/// Which sequence gets used is a judgment the window layer makes from what it observes of the composer and its own write
/// record, not something this pure layer decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitAction {
    /// Body then submit key, no clear sequence: the composer was observed
    /// empty.
    Write,
    /// Clear sequence, then body, then submit key: this call's own residue is
    /// still on screen but a different body is wanted.
    Rewrite,
    /// The submit key alone: this call's own envelope is still exactly what
    /// the composer holds, confirmed either on screen or by the write record.
    ResendOnly,
    /// The take-back alone, per [`InputProfile::take_back`]: taking back a
    /// write that never reflected, while it is still safe to do so.
    ClearOnly,
}

/// Builds the write sequence for one submit attempt. `line` is ignored by
/// [`SubmitAction::ResendOnly`], which never writes a body of its own (the
/// composer already holds the line, space included);
/// [`SubmitAction::ClearOnly`] never writes the line either: it writes the
/// profile's `clear` sequence alone.
///
/// [`SubmitAction::Write`] and [`SubmitAction::Rewrite`] always end with the
/// profile's submit key; [`SubmitAction::Rewrite`] prepends the clear
/// sequence only when the profile has one (an empty `clear` makes it
/// identical to `Write`). Every step but the last waits the profile's
/// `submit_delay` before the next. [`SubmitAction::ResendOnly`] is exactly
/// one step with no delay: there is nothing after it to wait for.
/// [`SubmitAction::ClearOnly`] is likewise one step with no delay and no
/// submit key, or no step at all when the profile has no `clear` sequence.
///
/// The line is framed with [`BRACKETED_PASTE_START`] /
/// [`BRACKETED_PASTE_END`] only when the profile asks for it *and* the line
/// holds no `ESC` byte: a line that could spell the end marker itself would
/// otherwise leave the frame, and the rest of it would be read as keystrokes.
/// The frame holds the whole line, its trailing space included. Apart from
/// that framing the line's bytes are the caller's body, unnormalized, plus
/// that space, so what an approver read is what the receiver is handed.
///
/// # Examples
///
/// ```
/// use yagura_daemon::input::{
///     BRACKETED_PASTE_END, BRACKETED_PASTE_START, ComposerLine, InputProfile, SubmitAction,
///     plan_submit_line,
/// };
///
/// let mut profile = InputProfile::default();
/// profile.bracketed_paste = true;
///
/// let framed = plan_submit_line(&profile, &ComposerLine::new("hello"), SubmitAction::Write);
/// let expected = [BRACKETED_PASTE_START, b"hello ", BRACKETED_PASTE_END].concat();
/// assert_eq!(framed.steps[0].bytes, expected);
///
/// let unframed = plan_submit_line(&profile, &ComposerLine::new("he\u{1b}llo"), SubmitAction::Write);
/// assert_eq!(unframed.steps[0].bytes, "he\u{1b}llo ".as_bytes());
/// ```
#[must_use]
pub fn plan_submit_line(
    profile: &InputProfile,
    line: &ComposerLine,
    action: SubmitAction,
) -> SubmitPlan {
    let steps = match action {
        SubmitAction::Write => vec![
            SubmitStep {
                bytes: frame_body(profile, line.as_str()),
                delay_after: profile.submit_delay,
            },
            SubmitStep {
                bytes: profile.submit.clone(),
                delay_after: Duration::ZERO,
            },
        ],
        SubmitAction::Rewrite => {
            let mut steps = Vec::with_capacity(3);
            if !profile.clear.is_empty() {
                steps.push(SubmitStep {
                    bytes: profile.clear.clone(),
                    delay_after: profile.submit_delay,
                });
            }
            steps.push(SubmitStep {
                bytes: frame_body(profile, line.as_str()),
                delay_after: profile.submit_delay,
            });
            steps.push(SubmitStep {
                bytes: profile.submit.clone(),
                delay_after: Duration::ZERO,
            });
            steps
        }
        SubmitAction::ResendOnly => vec![SubmitStep {
            bytes: profile.submit.clone(),
            delay_after: Duration::ZERO,
        }],
        SubmitAction::ClearOnly => {
            let bytes = profile.take_back_bytes();
            if bytes.is_empty() {
                Vec::new()
            } else {
                vec![SubmitStep {
                    bytes,
                    delay_after: Duration::ZERO,
                }]
            }
        }
    };
    SubmitPlan { steps }
}

/// Builds the plan for writing `text` through unchanged: one step, no
/// framing, no delay, no submit key of the daemon's own.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use yagura_daemon::input::plan_raw_write;
///
/// let plan = plan_raw_write("first\rsecond\r");
/// assert_eq!(plan.steps.len(), 1);
/// assert_eq!(plan.steps[0].bytes, b"first\rsecond\r");
/// assert_eq!(plan.steps[0].delay_after, Duration::ZERO);
/// ```
#[must_use]
pub fn plan_raw_write(text: &str) -> SubmitPlan {
    SubmitPlan {
        steps: vec![SubmitStep {
            bytes: text.as_bytes().to_vec(),
            delay_after: Duration::ZERO,
        }],
    }
}

/// The body step's bytes: framed as a paste when the profile asks for it and
/// the body cannot spell the end marker, and the body's own bytes otherwise.
fn frame_body(profile: &InputProfile, body: &str) -> Vec<u8> {
    let raw = body.as_bytes();
    if !profile.bracketed_paste || raw.contains(&ESC) {
        return raw.to_vec();
    }
    let mut framed =
        Vec::with_capacity(BRACKETED_PASTE_START.len() + raw.len() + BRACKETED_PASTE_END.len());
    framed.extend_from_slice(BRACKETED_PASTE_START);
    framed.extend_from_slice(raw);
    framed.extend_from_slice(BRACKETED_PASTE_END);
    framed
}
