//! `yagura-protocol`: the wire protocol shared between the yagura GUI (client)
//! and the yagura daemon.
//!
//! This crate has no I/O of its own. It only defines:
//! - the length-prefixed JSON framing (LPJF) used on top of the IPC transport
//!   (Windows named pipe / Unix domain socket), and
//! - the two message enums (`ClientMessage` for client-to-daemon, `DaemonMessage`
//!   for daemon-to-client) with their internally-tagged JSON representation.
//!
//! # How this wire may be extended
//!
//! The two sides of this wire are built and installed separately, so a client
//! regularly meets a daemon older than itself, and a daemon regularly meets an
//! older client. The three rules below are what keeps an extension from
//! breaking such a meeting. They are stated here and nowhere else; anything
//! that needs them points at this module instead of restating them.
//!
//! ## R1: client to daemon
//!
//! An extension that carries the caller's *intent* must not be added as a new
//! field on an existing [`ClientMessage`] variant. A daemon built before that
//! field decodes the message, drops the unknown key, and acts as though the
//! intent had never been expressed -- while the caller is told the request
//! succeeded. Give a new intent a new `type` instead, which an older daemon
//! refuses by name, or establish the peer's wire revision before sending it.
//!
//! ## R2: daemon to client
//!
//! Adding an optional field to a [`DaemonMessage`] is allowed, but its absence
//! must mean nothing beyond "the peer does not carry this field". A reader
//! that turns absence into a value -- a default, a denial, an empty result --
//! attributes that value to every peer older than the field, silently and
//! wrongly.
//!
//! ## R3: adding a value to an enum that travels on the wire
//!
//! Before adding a value, check whether that enum accepts strings it does not
//! know: `serde`'s catch-all attribute on a fallback variant is the only thing
//! that makes it do so, and each enum on this wire either carries it or does
//! not. An enum without it rejects an unknown string, and because these enums
//! sit inside messages, the rejection fails the decode of the whole frame --
//! a peer built before the value loses the entire message, not just the field.
//! So either the enum already has the catch-all variant, or the peer's wire
//! revision has to be established before a message carrying the new value is
//! sent.
//!
//! # Wire revisions
//!
//! [`PROTOCOL_REVISION`] names the wire this build implements, and the
//! protocol tests hold a frozen inventory of the wire for every revision ever
//! minted. Changing anything a peer can observe means raising the revision and
//! minting a new frozen inventory beside it.
//!
//! Revision 10 adds the model-alias messages (`list_model_choices`,
//! `save_model_aliases`, `add_model_choices` and their answers); a caller
//! establishes [`MODEL_ALIASES_MIN_REVISION`] before sending any of them.
//!
//! Revision 11 adds `set_profile_enabled` with its answer and the `enabled`
//! field of [`ProfileInfo`]; a caller establishes
//! [`PROFILE_ENABLED_MIN_REVISION`] before sending `set_profile_enabled`.
//!
//! Revision 12 adds `get_approval_settings`, `set_auto_approve` and
//! `set_plan_approval_required` with their answers, and the
//! [`ApprovalSettingsInfo`] / [`ToolAutoApprove`] / [`PlaybookState`] types
//! those answers carry; a caller establishes
//! [`APPROVAL_SETTINGS_MIN_REVISION`] before sending any of the three.
//!
//! Revision 13 adds `list_mcp_registrations` and `run_mcp_registration` with
//! their answers and pushes, and the `McpRegistration*` types they carry; a
//! caller establishes [`MCP_REGISTRATION_MIN_REVISION`] before sending either.
//!
//! Revision 14 adds `status_reports_missing` to [`WindowInfo`] and
//! `hook_entries` (of [`McpRegistrationHookEntry`]) to [`McpRegistrationRow`],
//! and drops `ttl_ms` from `status_event`. Both new fields are optional on
//! decode and a frame still carrying `ttl_ms` decodes with it ignored, so no
//! message needs a revision gate.
//!
//! Revision 15 drops `add_model_choices` and its answer: a profile's user file
//! is now laid over the shipped profile, so the shipped model list always
//! applies and there is nothing left to add.
//!
//! Revision 16 adds `get_profile_detail` and `set_profile_command` with their
//! answers, and the [`ProfileDetailEntry`] / [`ProfileCommandDetail`] types
//! they carry; a caller establishes [`PROFILE_DETAIL_MIN_REVISION`] before
//! sending either.
//!
//! Revision 17 replaces `attach` with `attach_at`, which carries the size the
//! caller will draw the window at, and renames `scrollback_b64` to `screen_b64`
//! in `attached` and `resync`: the daemon no longer replays raw output but
//! sends the window's terminal model as escape sequences. The old request is
//! a new `type` under R1, so a daemon built before this revision refuses
//! `attach_at` by name and a caller establishes revision 17
//! before sending it; the renamed answer fields are not readable by an older
//! client, which is why the revision is raised.
//!
//! Revision 18 adds the structured input messages (`key`, `text`, `paste`,
//! `pointer`, `wheel`, `focus`) and the types they carry: the daemon turns
//! them into bytes with the window's terminal modes. A caller establishes
//! [`STRUCTURED_INPUT_MIN_REVISION`] before sending any of them.
//!
//! Revision 19 makes the daemon the window's only terminal: `attach_at` is
//! renamed `attach_view`, `attached` loses `screen_b64`, and `output` and
//! `resync` give way to `frame`, which carries the window's cells as the
//! daemon's model holds them. The client acknowledges each frame
//! (`frame_ack`), sends its theme colors (`set_palette`), reports whether the
//! window is on screen (`set_visible`), asks for the selected text
//! (`copy_selection`, answered by `selection_text`), clears the selection
//! (`clear_selection`) and scrolls the view (`scroll_view`). A caller establishes [`FRAME_MIN_REVISION`]
//! before sending any of them; an older client's `attach_at` is refused by
//! name. Raw input bytes (`input`) are gone: a client sends input only as
//! the structured messages of revision 18.
//!
//! Revision 20 adds `get_daemon_health`, answered with `daemon_health`: the
//! problems that keep the daemon from opening windows. A caller establishes
//! [`DAEMON_HEALTH_MIN_REVISION`] before asking.
//!
//! Revision 21 drops `initial_window_failed` from [`DaemonProblemKind`]: the
//! daemon starts with no window, so there is no start-up window to fail. A
//! `status_event` without `window_id` is now refused as `invalid_argument`
//! instead of being attributed to the first window the daemon opened.
//!
//! Revision 22 adds the doors a CLI's hook reads a window's inbox through:
//! `deliver_context`, `deliver_take` and `deliver_wait`, answered with
//! `delivery`, and `report_session`, answered with `session_recorded`. A
//! caller establishes [`INBOX_MIN_REVISION`] before sending any of them.
//!
//! Revision 23 answers a handoff with `taken`, `undelivered_timeout` or
//! `undelivered_no_delivery` in place of what a composer was seen to do.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// The wire revision this build implements.
///
/// The value is a whole number that only ever increases, by one per change to
/// anything a peer can observe on the wire. It is what a peer reports when
/// asked which wire it speaks, and what a caller compares against before
/// sending something an older peer would mishandle.
pub const PROTOCOL_REVISION: u32 = 23;

/// The largest column count, and the largest row count, a window takes.
///
/// [`ClientMessage::Resize`] with `cols` outside
/// `MIN_WINDOW_COLS..=MAX_WINDOW_DIMENSION` or `rows` outside
/// `MIN_WINDOW_ROWS..=MAX_WINDOW_DIMENSION` is refused by the daemon and
/// changes nothing.
pub const MAX_WINDOW_DIMENSION: u16 = 1000;

/// The fewest columns a window takes: the narrowest screen the daemon's
/// terminal model can hold.
pub const MIN_WINDOW_COLS: u16 = 2;

/// The fewest rows a window takes.
pub const MIN_WINDOW_ROWS: u16 = 1;

/// The first wire revision that carries [`ClientMessage::SendKeys`].
pub const SEND_KEYS_MIN_REVISION: u32 = 6;

/// The wire revision attributed to a peer that reports none.
///
/// A peer built before revisions were reportable cannot name its own, so it is
/// credited with the oldest wire anyone has shipped. Everything that wire
/// could do keeps working against such a peer; only what a later revision
/// introduced is withheld.
pub const PROTOCOL_REVISION_BASELINE: u32 = 1;

/// The first wire revision at which a daemon honoring the `model` and
/// `effort` launch options can be *observed* to do so.
///
/// An older daemon may or may not read those fields, but it cannot say which,
/// and a caller has no way to tell the two apart; so this is the revision from
/// which honoring them becomes an observable fact rather than a guess.
pub const LAUNCH_OPTIONS_MIN_REVISION: u32 = 2;

/// The first wire revision at which a daemon storing and returning a plan
/// task's `instruction` can be *observed* to do so.
///
/// Mirrors [`LAUNCH_OPTIONS_MIN_REVISION`]'s reasoning: an older daemon
/// silently drops the field from `CreatePlan` / `UpdatePlan`, so a caller
/// establishes this revision before sending a task that carries one.
pub const TASK_INSTRUCTION_MIN_REVISION: u32 = 5;

/// The first wire revision at which the plan review gate
/// (`approve_plan` / `mark_plan_seen`, and a `save_roles` carrying a
/// `default_role`) is understood.
///
/// An older daemon refuses `approve_plan` / `mark_plan_seen` by name, and
/// silently drops a `default_role` it does not know from `save_roles`; so a
/// caller establishes this revision before sending either.
pub const PLAN_REVIEW_MIN_REVISION: u32 = 9;

/// The first wire revision at which the model-alias messages
/// (`list_model_choices`, `save_model_aliases`) are understood.
///
/// An older daemon refuses both by name, so a caller establishes this
/// revision before sending any of them.
pub const MODEL_ALIASES_MIN_REVISION: u32 = 10;

/// The first wire revision at which `set_profile_enabled` is understood and
/// [`ProfileInfo::enabled`] is carried.
///
/// An older daemon refuses `set_profile_enabled` by name, so a caller
/// establishes this revision before sending it.
pub const PROFILE_ENABLED_MIN_REVISION: u32 = 11;

/// The first wire revision at which `get_approval_settings`,
/// `set_auto_approve` and `set_plan_approval_required` are understood.
///
/// An older daemon refuses all three by name, so a caller establishes this
/// revision before sending any of them.
pub const APPROVAL_SETTINGS_MIN_REVISION: u32 = 12;

/// The first wire revision at which `list_mcp_registrations` and
/// `run_mcp_registration` are understood.
///
/// An older daemon refuses both by name, so a caller establishes this
/// revision before sending either.
pub const MCP_REGISTRATION_MIN_REVISION: u32 = 13;

/// The first wire revision at which `get_profile_detail` and
/// `set_profile_command` are understood.
///
/// An older daemon refuses both by name, so a caller establishes this
/// revision before sending either.
pub const PROFILE_DETAIL_MIN_REVISION: u32 = 16;

/// The first wire revision at which the structured input messages (`key`,
/// `text`, `paste`, `pointer`, `wheel`, `focus`) are understood.
///
/// An older daemon refuses them by name, so a caller establishes this
/// revision before sending any of them.
pub const STRUCTURED_INPUT_MIN_REVISION: u32 = 18;

/// The first wire revision at which `attach_view`, `frame`, `frame_ack`,
/// `set_palette`, `set_visible`, `copy_selection`, `clear_selection`,
/// `selection_text` and `scroll_view` exist.
///
/// An older daemon refuses the client messages by name, so a caller
/// establishes this revision before sending any of them.
pub const FRAME_MIN_REVISION: u32 = 19;

/// The first wire revision at which `get_daemon_health` is understood and
/// answered with `daemon_health`.
///
/// An older daemon refuses the request by name, and that refusal means it has
/// no problem to report, so a caller establishes this revision before asking.
pub const DAEMON_HEALTH_MIN_REVISION: u32 = 20;

/// The first wire revision that carries `deliver_context`, `deliver_take`,
/// `deliver_wait` and `report_session`.
///
/// An older daemon refuses them by name, so a hook command built for this
/// revision does nothing useful against it.
pub const INBOX_MIN_REVISION: u32 = 22;

/// The most JSON bytes (as [`encode_daemon_message`] produces them) one
/// [`Frame`] part may take; a larger frame is split into several parts by
/// row range.
pub const FRAME_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// The environment variable carrying a window's id to the process it runs.
pub const WINDOW_ID_ENV: &str = "YAGURA_WINDOW_ID";

/// The environment variable carrying a window's status token to the process
/// it runs. Its value is a credential and must never appear in an argv, a
/// file, or a log.
pub const STATUS_TOKEN_ENV: &str = "YAGURA_STATUS_TOKEN";

/// The environment variable naming the yagura instance a process belongs to.
pub const INSTANCE_ENV: &str = "YAGURA_INSTANCE";

/// The environment variable carrying the path of the MCP bridge executable
/// to a window's process, so a CLI's MCP configuration can start the bridge
/// without naming a path of its own.
pub const MCP_BRIDGE_ENV: &str = "YAGURA_MCP_BRIDGE";

/// The environment variables the MCP bridge reads to reach its window's
/// daemon and identify itself as that window, in this order.
pub const MCP_CHANNEL_ENV_VARS: [&str; 3] = [WINDOW_ID_ENV, STATUS_TOKEN_ENV, INSTANCE_ENV];

/// The fixed enumeration of a child process window's runtime states.
///
/// The values are transmitted over IPC as lowercase `snake_case` strings and
/// are vendor-neutral (no vendor-specific variants).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowStatus {
    /// The child process is currently running.
    Running,
    /// The child process is waiting for input from the user.
    WaitingInput,
    /// The child process is waiting for user approval or acknowledgement.
    WaitingApproval,
    /// The child process has been interrupted (e.g., by `Ctrl+C`).
    Interrupted,
    /// The child process is running but producing no output (quiescent).
    Idle,
    /// The child process has exited.
    Exited,
    /// The status is unknown or could not be determined.
    Unknown,
}

impl WindowStatus {
    /// Returns `true` if this status represents an actionable state.
    ///
    /// States that return `false` are `Unknown` and `Exited`. These states
    /// indicate either indeterminacy or finality, and should gate destructive
    /// operations.
    pub fn is_actionable(&self) -> bool {
        !matches!(self, Self::Unknown | Self::Exited)
    }
}

/// The reason [`DetectedStatus::Unknown`] carries when a window's status
/// cannot be determined more specifically.
///
/// Transmitted over IPC as a `snake_case` string and persisted to SQLite the
/// same way ([`UnknownReason::as_wire`] / [`UnknownReason::from_wire_str`]
/// are the single conversion point both uses share). A
/// reader that meets a string outside this set never guesses: `serde`
/// deserialization fails, and so does `from_wire_str`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    /// No usable detection definition is assigned to the window.
    NoDefinition,
    /// The screen matches no rule of the detection definition.
    NoRuleMatched,
    /// The screen model was lost and has not been trusted again.
    ScreenLost,
    /// The detection task stopped reporting for this window.
    DetectorStopped,
    /// The window just opened and nothing has been judged yet.
    NotYetJudged,
    /// A detection rule or a status event reported the status as unknown.
    DeclaredUnknown,
}

impl UnknownReason {
    /// The English phrase used in log lines and refusal messages (e.g. `"the
    /// window's status could not be detected ({phrase}), so this operation is
    /// refused"`).
    pub fn phrase(self) -> &'static str {
        match self {
            Self::NoDefinition => "no usable detection definition",
            Self::NoRuleMatched => "the screen matches no detection rule",
            Self::ScreenLost => "the screen model was lost",
            Self::DetectorStopped => "the detection task stopped",
            Self::NotYetJudged => "nothing has been judged yet",
            Self::DeclaredUnknown => "a rule or a status event reported unknown",
        }
    }

    /// The `snake_case` wire string this variant serializes to (matches
    /// `#[serde(rename_all = "snake_case")]`).
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::NoDefinition => "no_definition",
            Self::NoRuleMatched => "no_rule_matched",
            Self::ScreenLost => "screen_lost",
            Self::DetectorStopped => "detector_stopped",
            Self::NotYetJudged => "not_yet_judged",
            Self::DeclaredUnknown => "declared_unknown",
        }
    }

    /// The inverse of [`UnknownReason::as_wire`]. Returns `None` for any
    /// string outside the six wire forms, including one a newer build
    /// persisted that this build does not recognize (the caller, e.g. the
    /// chat delivery store, decides whether that means "no reason" or an
    /// error).
    pub fn from_wire_str(s: &str) -> Option<Self> {
        match s {
            "no_definition" => Some(Self::NoDefinition),
            "no_rule_matched" => Some(Self::NoRuleMatched),
            "screen_lost" => Some(Self::ScreenLost),
            "detector_stopped" => Some(Self::DetectorStopped),
            "not_yet_judged" => Some(Self::NotYetJudged),
            "declared_unknown" => Some(Self::DeclaredUnknown),
            _ => None,
        }
    }
}

/// A window's detected runtime status, together with why it is `Unknown`
/// when it is.
///
/// Distinct from [`WindowStatus`], which [`DetectedStatus::status`] projects
/// to and which every existing readiness / destructive-operation gate keeps
/// matching on. There is deliberately no function that reconstructs a
/// `DetectedStatus` from a transmitted `(status, unknown_reason)` pair: the
/// emitting side guarantees `unknown_reason().is_some()` iff `status() ==
/// WindowStatus::Unknown`, and that invariant is not re-derived by every
/// consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedStatus {
    /// See [`WindowStatus::Running`].
    Running,
    /// See [`WindowStatus::WaitingInput`].
    WaitingInput,
    /// See [`WindowStatus::WaitingApproval`].
    WaitingApproval,
    /// See [`WindowStatus::Interrupted`].
    Interrupted,
    /// See [`WindowStatus::Idle`].
    Idle,
    /// See [`WindowStatus::Exited`].
    Exited,
    /// See [`WindowStatus::Unknown`], carrying why.
    Unknown(UnknownReason),
}

impl DetectedStatus {
    /// Projects to the wire-level [`WindowStatus`], discarding the reason.
    pub fn status(self) -> WindowStatus {
        match self {
            Self::Running => WindowStatus::Running,
            Self::WaitingInput => WindowStatus::WaitingInput,
            Self::WaitingApproval => WindowStatus::WaitingApproval,
            Self::Interrupted => WindowStatus::Interrupted,
            Self::Idle => WindowStatus::Idle,
            Self::Exited => WindowStatus::Exited,
            Self::Unknown(_) => WindowStatus::Unknown,
        }
    }

    /// The carried reason. `Some` exactly when `self` is `Unknown`.
    pub fn unknown_reason(self) -> Option<UnknownReason> {
        match self {
            Self::Unknown(reason) => Some(reason),
            Self::Running
            | Self::WaitingInput
            | Self::WaitingApproval
            | Self::Interrupted
            | Self::Idle
            | Self::Exited => None,
        }
    }

    /// Builds the `DetectedStatus` for every `WindowStatus` other than
    /// `Unknown`. Returns `None` for `Unknown`, which carries an
    /// [`UnknownReason`] this function has no way to supply.
    pub fn known(status: WindowStatus) -> Option<Self> {
        match status {
            WindowStatus::Running => Some(Self::Running),
            WindowStatus::WaitingInput => Some(Self::WaitingInput),
            WindowStatus::WaitingApproval => Some(Self::WaitingApproval),
            WindowStatus::Interrupted => Some(Self::Interrupted),
            WindowStatus::Idle => Some(Self::Idle),
            WindowStatus::Exited => Some(Self::Exited),
            WindowStatus::Unknown => None,
        }
    }
}

impl From<DetectedStatus> for WindowStatus {
    fn from(value: DetectedStatus) -> Self {
        value.status()
    }
}

/// The single implementation of the `[a-z0-9_-]{1,64}` character-set rule
/// shared by every identifier held to it across the daemon and its wire
/// protocol: a `StatusEvent` source name ([`is_valid_source`]), a detection
/// definition name (`yagura_daemon::detector::definition::is_valid_detection_name`)
/// and a launch profile identifier (`yagura_daemon::profile::validate_profile_id`)
/// all delegate here rather than restating the rule.
///
/// A value must be non-empty, at most 64 bytes long, and made up only of
/// ASCII lowercase letters, digits, hyphens and underscores. Every validated
/// value ends up as a filesystem path component in at least one caller, so
/// this admits no path separator, `..`, uppercase letter or empty input.
pub fn is_valid_token(value: &str) -> bool {
    if value.is_empty() || value.len() > 64 {
        return false;
    }
    value
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Validates that a source name conforms to the allowed character set and length.
///
/// A valid source must:
/// - Be non-empty and at most 64 characters long
/// - Contain only ASCII lowercase letters, digits, hyphens, and underscores
///   (`[a-z0-9_-]`)
///
/// Delegates to [`is_valid_token`], shared by the receive and send sides so
/// the two cannot diverge.
pub fn is_valid_source(source: &str) -> bool {
    is_valid_token(source)
}

/// Maximum allowed length (in bytes) of a window display label.
///
/// The unit is UTF-8 bytes, not display columns: byte length is the only
/// measure the protocol layer can check without a font or locale, and it is
/// the same measure on both sides of the wire.
pub const WINDOW_NAME_MAX_BYTES: usize = 128;

/// Why an error enum rather than `()`: the daemon collapses every reason into
/// a single `invalid_argument` code on the wire, so a client that wants to
/// explain the rejection has to distinguish them before sending, and a unit
/// error type would force it to reimplement the rules.
///
/// "Label" covers both strings this rule guards: the window's display name
/// ([`validate_window_name`]) and its vendor badge
/// ([`validate_window_vendor`]), which the GUI renders side by side.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WindowNameError {
    /// The label is the empty string.
    #[error("window label must not be empty")]
    Empty,
    /// The label exceeds [`WINDOW_NAME_MAX_BYTES`].
    #[error("window label is {actual} bytes, exceeding the {max}-byte limit")]
    TooLong {
        /// The limit that was exceeded ([`WINDOW_NAME_MAX_BYTES`]).
        max: usize,
        /// The requested label's actual length in UTF-8 bytes.
        actual: usize,
    },
    /// The label contains a character that may not appear in a display label.
    ///
    /// The character itself is reported as a code point and never echoed
    /// verbatim in the message, so that a rejected label cannot smuggle
    /// control sequences into a log or a terminal through the error path.
    #[error("window label contains the forbidden character U+{:04X} at byte offset {at}", *ch as u32)]
    ForbiddenChar {
        /// Byte offset of the offending character within the label.
        at: usize,
        /// The offending character.
        ch: char,
    },
}

/// Characters that may not appear in a window display label.
///
/// These either break single-line rendering (controls, newlines, line and
/// paragraph separators), let a label reorder the text around it and
/// impersonate another window (bidirectional embeddings, overrides, isolates
/// and marks), or occupy no width at all, so that two different labels render
/// identically (zero-width space, word joiner, byte-order mark).
///
/// U+200C (ZWNJ) and U+200D (ZWJ) are *not* forbidden even though they are
/// equally invisible: they select which glyph is rendered,
/// so Persian, Urdu and Indic labels and emoji ZWJ sequences are wrong
/// without them. The other invisible characters only steer line breaking or
/// byte-order signalling, neither of which a single-line label needs, so
/// rejecting those costs no legitimate label.
fn is_forbidden_label_char(ch: char) -> bool {
    matches!(ch,
        // C0 controls (includes LF and CR) and DEL.
        '\u{0000}'..='\u{001F}' | '\u{007F}'
        // C1 controls.
        | '\u{0080}'..='\u{009F}'
        // Line and paragraph separators.
        | '\u{2028}' | '\u{2029}'
        // Zero-width space and the bidirectional marks (U+200C ZWNJ and
        // U+200D ZWJ sit between them and stay allowed).
        | '\u{200B}' | '\u{200E}' | '\u{200F}'
        // Word joiner and byte-order mark / zero-width no-break space.
        | '\u{2060}' | '\u{FEFF}'
        // Bidirectional embeddings / overrides and isolates.
        | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// Validates a window display name: non-empty, at most
/// [`WINDOW_NAME_MAX_BYTES`] UTF-8 bytes, and free of the characters listed
/// in [`WindowNameError::ForbiddenChar`]'s documentation.
///
/// This is the single source of truth for the rule, shared by the daemon
/// (which enforces it at every entry point that stores a name) and by
/// clients (which use it to explain a rejection before sending).
///
/// # Errors
///
/// Returns the first violation found, checked in the order empty ->
/// too long -> forbidden character.
pub fn validate_window_name(name: &str) -> Result<(), WindowNameError> {
    validate_label(name)
}

/// Validates a vendor identifier under the very same rule as
/// [`validate_window_name`].
///
/// The vendor is a display label too: the GUI renders it as a badge next to
/// the name in the window list, so a vendor value can reorder or shadow the
/// name it sits beside exactly as a name could. Sharing one implementation
/// rather than restating the rule is what keeps the two from drifting apart
/// and leaving one of the two labels unguarded.
///
/// # Errors
///
/// Same as [`validate_window_name`].
pub fn validate_window_vendor(vendor: &str) -> Result<(), WindowNameError> {
    validate_label(vendor)
}

/// Bytes a launch option value may hold; the same bound as a window name.
pub const LAUNCH_OPTION_VALUE_MAX_BYTES: usize = WINDOW_NAME_MAX_BYTES;

/// Why a `model` / `effort` value was rejected. A value becomes one argv
/// element of a launcher that may hand its arguments to a shell (`wsl --`,
/// a `.cmd` shim), so the rule admits only characters no launcher
/// reinterprets, and refuses a leading `-` so a value cannot inject a flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LaunchOptionValueError {
    /// The value is the empty string.
    #[error("launch option value is empty")]
    Empty,
    /// The value exceeds [`LAUNCH_OPTION_VALUE_MAX_BYTES`].
    #[error("launch option value exceeds {max} bytes (got {actual})")]
    TooLong {
        /// The limit that was exceeded ([`LAUNCH_OPTION_VALUE_MAX_BYTES`]).
        max: usize,
        /// The requested value's actual length in UTF-8 bytes.
        actual: usize,
    },
    /// The value contains a character outside the argv token set.
    ///
    /// The character itself is reported as a code point and never echoed
    /// verbatim in the message, for the same reason as
    /// [`WindowNameError::ForbiddenChar`].
    #[error(
        "launch option value contains U+{code:04X} at byte offset {at}; only ASCII letters, digits and . _ - : / + @ are allowed"
    )]
    ForbiddenChar {
        /// Byte offset of the offending character within the value.
        at: usize,
        /// The offending character's Unicode scalar value.
        code: u32,
    },
    /// The value starts with `-`, which a launcher could parse as a flag
    /// rather than the argument it is meant to be.
    #[error("launch option value must not start with '-'")]
    LeadingDash,
}

/// The characters a launch option value may consist of: the ASCII subset no
/// launcher, shell or config parser on the spawn path reinterprets.
fn is_launch_option_value_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':' | b'/' | b'+' | b'@')
}

/// Validates a launch option value (`model` / `effort`) against the argv
/// token rule: only ASCII letters, digits and `. _ - : / + @`, no leading
/// `-`, non-empty, at most [`LAUNCH_OPTION_VALUE_MAX_BYTES`] bytes.
///
/// This is a distinct rule and error type from [`validate_window_name`] (the
/// display-label rule): a launch option value ends up inside a process
/// argument that a launcher may hand to a shell (`wsl --`), not on screen, so
/// the two must not share an implementation. The daemon is the authority;
/// clients call this only to explain a rejection before sending.
///
/// # Errors
///
/// Returns the first violation found, checked in the order empty -> too
/// long -> leading dash -> forbidden character.
///
/// # Examples
///
/// ```
/// use yagura_protocol::{LaunchOptionValueError, validate_launch_option_value};
///
/// assert_eq!(validate_launch_option_value("tier-a"), Ok(()));
/// assert_eq!(validate_launch_option_value(""), Err(LaunchOptionValueError::Empty));
/// assert!(matches!(
///     validate_launch_option_value("a;b"),
///     Err(LaunchOptionValueError::ForbiddenChar { at: 1, .. })
/// ));
/// ```
pub fn validate_launch_option_value(value: &str) -> Result<(), LaunchOptionValueError> {
    if value.is_empty() {
        return Err(LaunchOptionValueError::Empty);
    }
    if value.len() > LAUNCH_OPTION_VALUE_MAX_BYTES {
        return Err(LaunchOptionValueError::TooLong {
            max: LAUNCH_OPTION_VALUE_MAX_BYTES,
            actual: value.len(),
        });
    }
    if value.starts_with('-') {
        return Err(LaunchOptionValueError::LeadingDash);
    }
    match value
        .char_indices()
        .find(|&(_, ch)| !ch.is_ascii() || !is_launch_option_value_byte(ch as u8))
    {
        Some((at, ch)) => Err(LaunchOptionValueError::ForbiddenChar {
            at,
            code: ch as u32,
        }),
        None => Ok(()),
    }
}

fn validate_label(label: &str) -> Result<(), WindowNameError> {
    if label.is_empty() {
        return Err(WindowNameError::Empty);
    }
    if label.len() > WINDOW_NAME_MAX_BYTES {
        return Err(WindowNameError::TooLong {
            max: WINDOW_NAME_MAX_BYTES,
            actual: label.len(),
        });
    }
    match label
        .char_indices()
        .find(|&(_, ch)| is_forbidden_label_char(ch))
    {
        Some((at, ch)) => Err(WindowNameError::ForbiddenChar { at, ch }),
        None => Ok(()),
    }
}

/// Errors rejecting one `argv` element: an empty string (a profile
/// authoring rule) or a NUL byte (a spawn invariant; see
/// [`validate_argv_nul_free`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ArgvElementError {
    /// An element is the empty string.
    #[error("an argv element is empty")]
    Empty,
    /// An element contains a NUL byte.
    #[error("an argv element contains a NUL byte")]
    Nul,
}

/// Validates that no element of `argv` contains a NUL byte: the single
/// implementation of the invariant every spawn path needs, since a NUL byte
/// cannot become part of an OS process argument.
///
/// # Errors
///
/// Returns [`ArgvElementError::Nul`] for the first element containing one.
pub fn validate_argv_nul_free(argv: &[String]) -> Result<(), ArgvElementError> {
    for element in argv {
        if element.contains('\0') {
            return Err(ArgvElementError::Nul);
        }
    }
    Ok(())
}

/// Validates every element of `argv` against a launch profile's authoring
/// rule: no element may be empty (checked first) or contain a NUL byte
/// (delegated to [`validate_argv_nul_free`]). An empty `argv` itself is not
/// rejected here: whether an `argv` may have zero elements at all is a
/// caller-level concern.
///
/// # Errors
///
/// Returns [`ArgvElementError::Empty`] for the first empty element, or
/// [`ArgvElementError::Nul`] for the first element containing a NUL byte.
pub fn validate_argv_elements(argv: &[String]) -> Result<(), ArgvElementError> {
    for element in argv {
        if element.is_empty() {
            return Err(ArgvElementError::Empty);
        }
    }
    validate_argv_nul_free(argv)
}

/// Errors rejecting an `open_window` request's `command` / `profile`
/// mutual-exclusion rule.
///
/// The single implementation shared by the MCP bridge
/// (`yagura_mcp::tools::open_window`) and the Tauri command
/// (`gui/src-tauri/src/commands.rs::open_window`), so the two call sites
/// cannot restate the rule and drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OpenWindowExclusivityError {
    /// `profile` was given as the empty string, distinct from `None`.
    #[error("profile must be a non-empty string")]
    EmptyProfile,
    /// Both `command` and `profile` were provided.
    #[error("command and profile are mutually exclusive")]
    BothProvided,
    /// Neither `command` nor `profile` was provided.
    #[error("either command or profile must be provided")]
    NeitherProvided,
    /// `detection` was provided alongside `profile`.
    #[error("detection cannot be specified when profile is provided")]
    DetectionWithProfile,
    /// `vendor` was provided alongside `profile`.
    #[error("vendor cannot be specified when profile is provided")]
    VendorWithProfile,
    /// A launch option (`model` / `effort`) was provided without `profile`.
    /// Only a profile carries the mapping that turns the value into argv.
    #[error("model and effort can only be specified with profile")]
    LaunchOptionWithoutProfile,
}

/// Validates an `open_window` request's `command` / `profile` mutual
/// exclusion, checked in this fixed order: an empty `profile` string first
/// (regardless of `has_command`), then both provided, then -- only once a
/// `profile` is present -- `detection` and `vendor` in that order, then
/// neither provided, then a launch option without a profile.
///
/// `has_launch_option` is whether `model` or `effort` was supplied.
///
/// # Errors
///
/// Returns the first violation found in the order documented above:
/// [`OpenWindowExclusivityError::EmptyProfile`],
/// [`OpenWindowExclusivityError::BothProvided`],
/// [`OpenWindowExclusivityError::DetectionWithProfile`],
/// [`OpenWindowExclusivityError::VendorWithProfile`],
/// [`OpenWindowExclusivityError::NeitherProvided`] or
/// [`OpenWindowExclusivityError::LaunchOptionWithoutProfile`].
///
/// # Examples
///
/// ```
/// use yagura_protocol::{OpenWindowExclusivityError, validate_open_window_exclusivity};
///
/// assert_eq!(
///     validate_open_window_exclusivity(false, Some("alpha"), None, None, true),
///     Ok(())
/// );
/// assert_eq!(
///     validate_open_window_exclusivity(true, None, None, None, true),
///     Err(OpenWindowExclusivityError::LaunchOptionWithoutProfile)
/// );
/// ```
pub fn validate_open_window_exclusivity(
    has_command: bool,
    profile: Option<&str>,
    detection: Option<&str>,
    vendor: Option<&str>,
    has_launch_option: bool,
) -> Result<(), OpenWindowExclusivityError> {
    if profile == Some("") {
        return Err(OpenWindowExclusivityError::EmptyProfile);
    }
    if has_command && profile.is_some() {
        return Err(OpenWindowExclusivityError::BothProvided);
    }
    if profile.is_some() {
        if detection.is_some() {
            return Err(OpenWindowExclusivityError::DetectionWithProfile);
        }
        if vendor.is_some() {
            return Err(OpenWindowExclusivityError::VendorWithProfile);
        }
        return Ok(());
    }
    if !has_command {
        return Err(OpenWindowExclusivityError::NeitherProvided);
    }
    if has_launch_option {
        return Err(OpenWindowExclusivityError::LaunchOptionWithoutProfile);
    }
    Ok(())
}

/// Upper bound (bytes) on the body a request awaiting approval may carry: the
/// `send_message` text, or the `send_handoff` envelope.
///
/// Far below any frame limit, because an approver is shown the body in full
/// and has to read it before deciding, so a body no human can read is refused
/// rather than queued.
pub const APPROVAL_TEXT_MAX_BYTES: usize = 2048;

/// Upper bound (bytes) on a handoff's `reference`: what the receiving window
/// is asked to look at.
pub const HANDOFF_REFERENCE_MAX_BYTES: usize = 512;

/// Upper bound (bytes) on a handoff's `note`: the requester's own words.
pub const HANDOFF_NOTE_MAX_BYTES: usize = 1024;

/// Upper bound (bytes) on a whole handoff envelope, the text written into the
/// receiving window's PTY.
///
/// Shares its value with [`APPROVAL_TEXT_MAX_BYTES`] rather than restating a
/// number: an envelope is exactly what an approver has to read before letting
/// the handoff through, so the two bounds are one requirement.
pub const HANDOFF_ENVELOPE_MAX_BYTES: usize = APPROVAL_TEXT_MAX_BYTES;

/// Upper bound (bytes) on a task report's `summary`: the reporting window's
/// own account of how the work ended.
pub const REPORT_SUMMARY_MAX_BYTES: usize = 1024;

/// Upper bound (bytes) on a task report's `path`: where a longer result was
/// written.
pub const REPORT_PATH_MAX_BYTES: usize = 512;

/// Upper bound on the number of named keys in one `send_keys` request.
pub const SEND_KEYS_MAX_KEYS: usize = 16;

/// Why a handoff's target arguments were rejected.
///
/// The daemon collapses every reason into a single `invalid_argument` code on
/// the wire, so a client that wants to explain the rejection has to
/// distinguish them before sending. Sharing one enum is what keeps the MCP
/// bridge and the daemon from restating the rule and drifting apart.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HandoffArgError {
    /// Neither `window_id` nor `profile` was provided.
    #[error("either window_id or profile must be provided")]
    NeitherProvided,
    /// Both `window_id` and `profile` were provided.
    #[error("window_id and profile are mutually exclusive")]
    BothProvided,
    /// `cwd` was provided alongside `window_id`. A `cwd` only means anything
    /// for a window this handoff is about to open.
    #[error("cwd cannot be specified when window_id is provided")]
    WindowIdWithCwd,
    /// `name` was provided alongside `window_id`. Renaming an existing window
    /// is `rename_window`'s business, not a handoff's.
    #[error("name cannot be specified when window_id is provided")]
    WindowIdWithName,
    /// A launch option (`model` / `effort`) was provided alongside
    /// `window_id`. A running window was launched with whatever options it
    /// was launched with; a value here has nothing to act on.
    #[error("model and effort cannot be specified with window_id")]
    WindowIdWithLaunchOption,
    /// `profile` is not a valid profile identifier ([`is_valid_token`]).
    /// Whether a profile of that name exists is a separate, later question.
    #[error("profile must match the [a-z0-9_-]{{1,64}} identifier rule")]
    Profile,
    /// `name` is not a valid window display label ([`validate_window_name`]).
    #[error("name is not a valid window label: {0}")]
    Name(WindowNameError),
}

/// Why a handoff's `reference` or `note` was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HandoffTextError {
    /// The value is the empty string.
    #[error("handoff text must not be empty")]
    Empty,
    /// The value exceeds the caller-supplied byte limit.
    #[error("handoff text is {actual} bytes, exceeding the {max}-byte limit")]
    TooLong {
        /// The limit that was exceeded.
        max: usize,
        /// The value's actual length in UTF-8 bytes.
        actual: usize,
    },
    /// The value contains a character a handoff text may not carry: the same
    /// characters as [`WindowNameError::ForbiddenChar`], whose documentation
    /// is the single description of that set. A handoff text is written into
    /// a terminal and shown to an approver, so it is held to the rule that
    /// keeps a label on one line and free of text-reordering controls.
    ///
    /// The character itself is reported as a code point and never echoed
    /// verbatim in the message, so that a rejected value cannot smuggle
    /// control sequences into a log or a terminal through the error path.
    #[error("handoff text contains the forbidden character U+{:04X} at byte offset {at}", *ch as u32)]
    ForbiddenChar {
        /// Byte offset of the offending character within the value.
        at: usize,
        /// The offending character.
        ch: char,
    },
}

/// Validates a handoff's target arguments: exactly one of `window_id` and
/// `profile`, with `cwd`, `name` and the launch options reserved for the
/// `profile` form, and `name` held to the same rule as any other window
/// display label.
///
/// Checked in this fixed order: neither provided -> both provided ->
/// `window_id` with `cwd` -> `window_id` with `name` -> `window_id` with a
/// launch option -> `profile`'s identifier rule -> `name`'s label rule.
///
/// `has_launch_option` is whether `model` or `effort` was supplied.
///
/// # Errors
///
/// Returns the first violation found in the order documented above:
/// [`HandoffArgError::NeitherProvided`], [`HandoffArgError::BothProvided`],
/// [`HandoffArgError::WindowIdWithCwd`], [`HandoffArgError::WindowIdWithName`],
/// [`HandoffArgError::WindowIdWithLaunchOption`], [`HandoffArgError::Profile`]
/// or [`HandoffArgError::Name`].
///
/// # Examples
///
/// ```
/// use yagura_protocol::{HandoffArgError, validate_handoff_target};
///
/// assert_eq!(validate_handoff_target(Some("w1"), None, None, None, false), Ok(()));
/// assert_eq!(
///     validate_handoff_target(None, Some("alpha"), Some("/work"), Some("reviewer"), true),
///     Ok(())
/// );
/// assert_eq!(
///     validate_handoff_target(Some("w1"), Some("alpha"), None, None, false),
///     Err(HandoffArgError::BothProvided)
/// );
/// assert_eq!(
///     validate_handoff_target(Some("w1"), None, Some("/work"), None, false),
///     Err(HandoffArgError::WindowIdWithCwd)
/// );
/// assert_eq!(
///     validate_handoff_target(Some("w1"), None, None, None, true),
///     Err(HandoffArgError::WindowIdWithLaunchOption)
/// );
/// ```
pub fn validate_handoff_target(
    window_id: Option<&str>,
    profile: Option<&str>,
    cwd: Option<&str>,
    name: Option<&str>,
    has_launch_option: bool,
) -> Result<(), HandoffArgError> {
    if window_id.is_none() && profile.is_none() {
        return Err(HandoffArgError::NeitherProvided);
    }
    if window_id.is_some() && profile.is_some() {
        return Err(HandoffArgError::BothProvided);
    }
    if window_id.is_some() {
        if cwd.is_some() {
            return Err(HandoffArgError::WindowIdWithCwd);
        }
        if name.is_some() {
            return Err(HandoffArgError::WindowIdWithName);
        }
        if has_launch_option {
            return Err(HandoffArgError::WindowIdWithLaunchOption);
        }
    }
    if profile.is_some_and(|profile| !is_valid_token(profile)) {
        return Err(HandoffArgError::Profile);
    }
    if let Some(name) = name {
        validate_window_name(name).map_err(HandoffArgError::Name)?;
    }
    Ok(())
}

/// Validates one of a handoff's two free-text fields: non-empty, at most
/// `max_bytes` UTF-8 bytes, and free of the characters listed in
/// [`HandoffTextError::ForbiddenChar`]'s documentation.
///
/// `max_bytes` is the caller's, because the two fields carry different
/// budgets ([`HANDOFF_REFERENCE_MAX_BYTES`] and [`HANDOFF_NOTE_MAX_BYTES`])
/// under one rule.
///
/// # Errors
///
/// Returns the first violation found, checked in the order empty ->
/// too long -> forbidden character: [`HandoffTextError::Empty`],
/// [`HandoffTextError::TooLong`] or [`HandoffTextError::ForbiddenChar`].
///
/// # Examples
///
/// ```
/// use yagura_protocol::{HANDOFF_NOTE_MAX_BYTES, HandoffTextError, validate_handoff_text};
///
/// assert_eq!(
///     validate_handoff_text("look at src/foo.rs:42 (the error path)", HANDOFF_NOTE_MAX_BYTES),
///     Ok(())
/// );
/// assert_eq!(
///     validate_handoff_text("", HANDOFF_NOTE_MAX_BYTES),
///     Err(HandoffTextError::Empty)
/// );
/// assert!(matches!(
///     validate_handoff_text("one\ntwo", HANDOFF_NOTE_MAX_BYTES),
///     Err(HandoffTextError::ForbiddenChar { at: 3, .. })
/// ));
/// ```
pub fn validate_handoff_text(value: &str, max_bytes: usize) -> Result<(), HandoffTextError> {
    if value.is_empty() {
        return Err(HandoffTextError::Empty);
    }
    if value.len() > max_bytes {
        return Err(HandoffTextError::TooLong {
            max: max_bytes,
            actual: value.len(),
        });
    }
    match value
        .char_indices()
        .find(|&(_, ch)| is_forbidden_label_char(ch))
    {
        Some((at, ch)) => Err(HandoffTextError::ForbiddenChar { at, ch }),
        None => Ok(()),
    }
}

/// Upper bound (bytes) on a plan task's `instruction`.
///
/// The value is returned only through `get_plan`'s JSON reply, never written
/// into a window's PTY, so it is held to neither the input-box budget
/// ([`APPROVAL_TEXT_MAX_BYTES`]) nor a launcher argument's; the limit exists
/// only to keep one task's request, and a plan's total reply size
/// (`MAX_PLAN_TASKS` tasks each near this bound), well inside
/// [`MAX_FRAME_BODY_LEN`].
pub const TASK_INSTRUCTION_MAX_BYTES: usize = 16 * 1024;

/// Why a plan task's `instruction` was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TaskInstructionError {
    /// The value is the empty string.
    #[error("task instruction must not be empty")]
    Empty,
    /// The value exceeds [`TASK_INSTRUCTION_MAX_BYTES`].
    #[error("task instruction is {len} bytes, exceeding the {max}-byte limit")]
    TooLong {
        /// The requested value's actual length in UTF-8 bytes.
        len: usize,
        /// The limit that was exceeded ([`TASK_INSTRUCTION_MAX_BYTES`]).
        max: usize,
    },
    /// The value contains a character an instruction may not carry: the same
    /// set [`WindowNameError::ForbiddenChar`]'s documentation lists, except
    /// `\n` and `\t` (see [`is_forbidden_instruction_char`]). Unlike
    /// [`validate_send_text_body`], `\r` stays forbidden here: a caller
    /// normalizes `\r` to `\n` before this validator ever sees the value.
    ///
    /// The character itself is reported as a code point and never echoed
    /// verbatim in the message, so that a rejected instruction cannot
    /// smuggle control sequences into a log through the error path.
    #[error("task instruction contains the forbidden character U+{:04X} at byte offset {at}", *ch as u32)]
    ForbiddenChar {
        /// Byte offset of the offending character within the value.
        at: usize,
        /// The offending character.
        ch: char,
    },
}

/// Same set as [`is_forbidden_label_char`], except `\n` and `\t`: an
/// instruction is prose with its own line structure, which needs both to
/// stay readable.
fn is_forbidden_instruction_char(ch: char) -> bool {
    !matches!(ch, '\n' | '\t') && is_forbidden_label_char(ch)
}

/// Validates a plan task's `instruction`: non-empty, at most
/// [`TASK_INSTRUCTION_MAX_BYTES`] UTF-8 bytes, and free of the characters
/// listed in [`TaskInstructionError::ForbiddenChar`]'s documentation.
///
/// # Errors
///
/// Returns the first violation found, checked in the order empty -> too
/// long -> forbidden character: [`TaskInstructionError::Empty`],
/// [`TaskInstructionError::TooLong`] or [`TaskInstructionError::ForbiddenChar`].
///
/// # Examples
///
/// ```
/// use yagura_protocol::{TaskInstructionError, validate_task_instruction};
///
/// assert_eq!(
///     validate_task_instruction("read src/foo.rs, then write a test for it"),
///     Ok(())
/// );
/// assert_eq!(validate_task_instruction(""), Err(TaskInstructionError::Empty));
/// assert!(matches!(
///     validate_task_instruction("one\rtwo"),
///     Err(TaskInstructionError::ForbiddenChar { at: 3, .. })
/// ));
/// ```
pub fn validate_task_instruction(value: &str) -> Result<(), TaskInstructionError> {
    if value.is_empty() {
        return Err(TaskInstructionError::Empty);
    }
    if value.len() > TASK_INSTRUCTION_MAX_BYTES {
        return Err(TaskInstructionError::TooLong {
            len: value.len(),
            max: TASK_INSTRUCTION_MAX_BYTES,
        });
    }
    match value
        .char_indices()
        .find(|&(_, ch)| is_forbidden_instruction_char(ch))
    {
        Some((at, ch)) => Err(TaskInstructionError::ForbiddenChar { at, ch }),
        None => Ok(()),
    }
}

/// Same set as [`is_forbidden_label_char`], except `\n` and `\r`: a
/// `send_message` body uses them as its own submit marker (a single
/// trailing terminator), so they are not control characters this rule
/// rejects outright -- whether one is *placed* legally is
/// [`validate_send_text_body`]'s separate embedded-line-break check.
fn is_forbidden_send_text_char(ch: char) -> bool {
    !matches!(ch, '\n' | '\r') && is_forbidden_label_char(ch)
}

/// Why a `send_message` body was rejected.
///
/// A distinct type from [`HandoffTextError`] because the two rules differ:
/// a `send_message` body's size budget is the daemon's own `send_text` limit
/// rather than one of this crate's text-field byte budgets, and an empty
/// body is a valid (no-op) send, so this type carries only the violations
/// the two rules do share, plus the one below that is unique to a body
/// written straight into a terminal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SendTextError {
    /// The value contains a character forbidden in a `send_message` body:
    /// the same set [`HandoffTextError::ForbiddenChar`] documents, except
    /// `\n` and `\r` (see [`is_forbidden_send_text_char`]).
    ///
    /// The character itself is reported as a code point and never echoed
    /// verbatim in the message, for the same reason as
    /// [`HandoffTextError::ForbiddenChar`].
    #[error("send_message text contains the forbidden character U+{:04X} at byte offset {at}", *ch as u32)]
    ForbiddenChar {
        /// Byte offset of the offending character within the value.
        at: usize,
        /// The offending character.
        ch: char,
    },
    /// The value carries a `\r` or `\n` that is not its single trailing
    /// terminator: the daemon writes a `send_message` body straight through
    /// with no per-line confirmation, so it cannot tell whether a body with
    /// an embedded line break reached the receiver as more than one message.
    /// Rather than report a false success, the body is rejected outright; a
    /// caller with several lines to deliver sends them one at a time, or
    /// writes them into a plan task's `instruction` (see
    /// [`validate_task_instruction`]).
    #[error(
        "send_message text contains a line break at byte offset {at} that is not the body's single trailing terminator"
    )]
    EmbeddedLineBreak {
        /// Byte offset of the first line break that is not the body's
        /// single trailing terminator.
        at: usize,
    },
}

/// A named control key that may be injected into a window.
///
/// The set is deliberately closed: callers cannot use this operation to send
/// arbitrary bytes or terminal escape sequences. `Unknown` is a serde
/// catch-all so the daemon can return `invalid_argument` for a newer key name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKey {
    Escape,
    Backspace,
    CtrlU,
    Enter,
    Up,
    Down,
    #[serde(other)]
    Unknown,
}

impl ControlKey {
    /// The stable wire spelling used in approval text and validation errors.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Escape => "escape",
            Self::Backspace => "backspace",
            Self::CtrlU => "ctrl_u",
            Self::Enter => "enter",
            Self::Up => "up",
            Self::Down => "down",
            Self::Unknown => "unknown",
        }
    }
}

/// Why a `send_keys` request was rejected before its approval gate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SendKeysError {
    #[error("keys must contain at least one key")]
    Empty,
    #[error("keys must contain at most {max} keys", max = SEND_KEYS_MAX_KEYS)]
    TooMany,
    #[error("escape must be the last key")]
    EscapeMustBeLast,
    #[error("unknown control key")]
    Unknown,
}

/// Validates a named-key sequence for `send_keys`.
///
/// The bytes a key becomes depend on the window's terminal modes at the
/// moment it is written, so they are the daemon's to produce, not this
/// crate's.
///
/// # Errors
///
/// [`SendKeysError::Empty`] for no keys, [`SendKeysError::TooMany`] for more
/// than [`SEND_KEYS_MAX_KEYS`], [`SendKeysError::EscapeMustBeLast`] for an
/// `Escape` before the last key, and [`SendKeysError::Unknown`] for a key
/// name this build does not know.
pub fn validate_control_keys(keys: &[ControlKey]) -> Result<(), SendKeysError> {
    if keys.is_empty() {
        return Err(SendKeysError::Empty);
    }
    if keys.len() > SEND_KEYS_MAX_KEYS {
        return Err(SendKeysError::TooMany);
    }
    for (index, key) in keys.iter().enumerate() {
        if *key == ControlKey::Escape && index + 1 != keys.len() {
            return Err(SendKeysError::EscapeMustBeLast);
        }
        if *key == ControlKey::Unknown {
            return Err(SendKeysError::Unknown);
        }
    }
    Ok(())
}

/// The most bytes one [`ClientMessage::Text`] may carry. Committed text is
/// what one keystroke or one IME commit produces, far below this.
pub const MAX_TEXT_INPUT_BYTES: usize = 4096;

/// The most bytes one [`ClientMessage::Paste`] may carry. One mebibyte stays
/// well inside the frame limit even after JSON escaping.
pub const MAX_PASTE_BYTES: usize = 1024 * 1024;

/// The most lines one [`ClientMessage::Wheel`] may move, either way.
pub const MAX_WHEEL_LINES: u16 = 100;

/// Checks `msg` against the size bounds of structured input, the one check
/// every sender and the daemon make. Messages that are not sized input pass:
/// whether a message is input at all is the caller's to decide.
///
/// # Errors
///
/// The reason the input is refused: a [`ClientMessage::Text`] past
/// [`MAX_TEXT_INPUT_BYTES`], a [`ClientMessage::Paste`] past
/// [`MAX_PASTE_BYTES`], or a [`ClientMessage::Wheel`] past
/// [`MAX_WHEEL_LINES`] either way.
pub fn check_input_bounds(msg: &ClientMessage) -> Result<(), &'static str> {
    match msg {
        ClientMessage::Text { text } if text.len() > MAX_TEXT_INPUT_BYTES => {
            Err("text exceeds the input limit")
        }
        ClientMessage::Paste { text } if text.len() > MAX_PASTE_BYTES => {
            Err("paste exceeds the paste limit")
        }
        ClientMessage::Wheel(wheel) if wheel.delta.unsigned_abs() > u32::from(MAX_WHEEL_LINES) => {
            Err("wheel delta exceeds the line limit")
        }
        _ => Ok(()),
    }
}

/// The modifier keys held during an input event. Meta is not carried: a
/// terminal writes nothing of its own for it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modifiers {
    /// Shift is held.
    #[serde(default)]
    pub shift: bool,
    /// Alt (Option) is held.
    #[serde(default)]
    pub alt: bool,
    /// Ctrl is held.
    #[serde(default)]
    pub ctrl: bool,
}

/// A key that prints nothing by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedKey {
    Enter,
    Tab,
    Backspace,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// The key of a [`KeyInput`]: a named key, or one character with Shift
/// already applied (Shift+a is `'A'`), as a keyboard layout produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    /// A key that prints nothing by itself.
    Named(NamedKey),
    /// A character key.
    Char(char),
}

/// One key press: [`ClientMessage::Key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyInput {
    /// The key pressed.
    pub key: Key,
    /// The modifiers held.
    #[serde(default)]
    pub mods: Modifiers,
}

/// A cell of the visible screen, counted from 0 at the top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellPosition {
    /// The column.
    pub col: u16,
    /// The row.
    pub row: u16,
}

/// What a pointer did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerKind {
    /// A button went down.
    Down,
    /// A button went up.
    Up,
    /// The pointer moved to another cell.
    Move,
}

/// The pointer button of an event: the one pressed or released, or the one
/// held while moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerButton {
    Left,
    Middle,
    Right,
    /// No button (a move with nothing held).
    None,
}

/// One pointer event: [`ClientMessage::Pointer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerInput {
    /// What the pointer did.
    pub kind: PointerKind,
    /// The button involved.
    pub button: PointerButton,
    /// The cell under the pointer.
    pub cell: CellPosition,
    /// The modifiers held.
    #[serde(default)]
    pub mods: Modifiers,
    /// The click count of a press (2 for a double click).
    #[serde(default = "single_click")]
    pub clicks: u8,
}

fn single_click() -> u8 {
    1
}

/// One wheel event: [`ClientMessage::Wheel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WheelInput {
    /// Lines to move; negative is up. At most [`MAX_WHEEL_LINES`] either way.
    pub delta: i32,
    /// The cell under the pointer.
    pub cell: CellPosition,
    /// The modifiers held.
    #[serde(default)]
    pub mods: Modifiers,
}

/// The byte length of `value`'s single trailing terminator, if it has one:
/// `2` for a trailing `"\r\n"`, `1` for a trailing lone `"\n"` or `"\r"`, `0`
/// otherwise. The single place [`validate_send_text_body`] decides which
/// `\r` / `\n` in a body is "the" terminator rather than an embedded line
/// break.
fn trailing_terminator_len(value: &str) -> usize {
    if value.ends_with("\r\n") {
        2
    } else if value.ends_with('\n') || value.ends_with('\r') {
        1
    } else {
        0
    }
}

/// Validates a `send_message` body against the same forbidden-character rule
/// as [`validate_handoff_text`] (except that `\n` and `\r` are allowed, see
/// [`is_forbidden_send_text_char`]), plus the rule that a `\r` or `\n` may
/// appear only once, at the very end, as the body's own submit marker:
/// a body's other control characters (in particular `ESC`) could
/// redraw the receiving window's screen and spoof the daemon's own read of
/// it, which is the vulnerability the forbidden-character rule closes.
///
/// # Errors
///
/// Returns [`SendTextError::ForbiddenChar`] for the first character outside
/// the allowed set, or [`SendTextError::EmbeddedLineBreak`] for the first
/// `\r` / `\n` that is not the body's single trailing terminator.
///
/// # Examples
///
/// ```
/// use yagura_protocol::{SendTextError, validate_send_text_body};
///
/// assert_eq!(validate_send_text_body("run the tests\n"), Ok(()));
/// assert!(matches!(
///     validate_send_text_body("first\rsecond\r"),
///     Err(SendTextError::EmbeddedLineBreak { at: 5 })
/// ));
/// assert!(matches!(
///     validate_send_text_body("\x1b[2Jerased"),
///     Err(SendTextError::ForbiddenChar { at: 0, .. })
/// ));
/// ```
pub fn validate_send_text_body(value: &str) -> Result<(), SendTextError> {
    let terminator_start = value.len() - trailing_terminator_len(value);
    for (at, ch) in value.char_indices() {
        if ch == '\n' || ch == '\r' {
            if at < terminator_start {
                return Err(SendTextError::EmbeddedLineBreak { at });
            }
            continue;
        }
        if is_forbidden_send_text_char(ch) {
            return Err(SendTextError::ForbiddenChar { at, ch });
        }
    }
    Ok(())
}

/// Maximum allowed length (in bytes) of a single frame body.
///
/// A declared length prefix exactly equal to this value is accepted (the body
/// is simply awaited); only a declared length *exceeding* this value is
/// rejected with [`FrameError::TooLarge`].
pub const MAX_FRAME_BODY_LEN: usize = 16 * 1024 * 1024;

/// Window metadata shared across wire protocol responses (multi-window v2).
///
/// The single source of truth for window information transmitted in responses
/// like `WindowList` and other window queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Unique identifier for this window within the daemon process lifetime.
    /// Window IDs are non-persistent: they are valid only while the daemon
    /// process is running.
    pub window_id: String,
    /// User-provided or auto-derived name for the window.
    pub name: String,
    /// Optional vendor identifier (e.g., `"claude-code"`, `"codex"`).
    /// Vendors are transmitted as data values, not type variants.
    pub vendor: Option<String>,
    /// Absolute path to the working directory (always resolved, never relative).
    pub cwd: String,
    /// Current status of the child process.
    pub status: WindowStatus,
    /// Exit code if the child has exited, `None` if still running.
    pub exit_code: Option<i32>,
    /// The window's presentation layout, as the opaque JSON string the GUI
    /// last reported through [`ClientMessage::UpdateWindowLayout`]. Omitted
    /// from the wire when absent, so a peer predating this field still
    /// decodes (and produces) a valid `WindowInfo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout_json: Option<String>,
    /// The room this window belongs to, filled in only by `WindowRegistry::list`
    /// (a single lookup by id carries `None`). `None` for a window whose
    /// status is `Exited`. Omitted from the wire when absent, so a peer
    /// predating this field still decodes (and produces) a valid `WindowInfo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<RoomRef>,
    /// Why `status` is `Unknown`, if it is. `None` when `status` is not
    /// `Unknown`, or when the daemon predates this field. Omitted from the
    /// wire when `None`, so a peer predating this field still decodes (and
    /// produces) a valid `WindowInfo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_reason: Option<UnknownReason>,
    /// English reason text carried alongside `unknown_reason` when the window
    /// runs no detection definition: one failed to load, or no definition
    /// layer is configured at all (the same text
    /// [`DaemonMessage::WindowOpened::detection_warning`] reports at open
    /// time). `None` when a definition loaded. Omitted from the wire when
    /// `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_warning: Option<String>,
    /// The generation of the status pair this record reports: a counter the
    /// daemon increases once per confirmed transition of this window, so a
    /// reader whose registry this record updates can discard an observation
    /// older than one it already applied. Per window, never reused, and never
    /// compared across windows. `None` when the daemon predates this field,
    /// which a reader treats as "always apply". Omitted from the wire when
    /// `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_rev: Option<u64>,
    /// Whether an MCP bridge has identified itself as this window over a
    /// connection that is still open. `None` when the daemon does not report
    /// this fact, which a reader must not read as "not connected". Omitted
    /// from the wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_connected: Option<bool>,
    /// Whether this window's CLI is expected to report its turns but the
    /// screen has shown a running turn for a grace time without one. `None`
    /// when the daemon does not report this fact, which a reader must not
    /// read as "reports arriving". Omitted from the wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_reports_missing: Option<bool>,
}

/// One restore candidate: a window from the previous boot's configuration,
/// as read back from the daemon's session store.
///
/// Vendor-neutral by construction (the vendor is a data value, never a type
/// variant) and **untrusted**: the store is a file another process running as
/// the same user can write, so a client must validate these values before
/// acting on them and sanitize them before display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreCandidate {
    /// 0-based position within the stored configuration, ascending in the
    /// order the windows were opened. Also the order the restore UI presents
    /// the candidates in.
    pub position: i64,
    /// Argument vector the window was spawned with.
    pub command: Vec<String>,
    /// Working directory, as resolved when the window was opened.
    pub cwd: String,
    /// Display name.
    pub name: String,
    /// Vendor label, or `None` when the window had none.
    pub vendor: Option<String>,
    /// Detection definition name, verbatim from the original open request.
    pub detection: Option<String>,
    /// The candidate's presentation layout, opaque to the daemon. `None`
    /// when the window had no layout saved, or when the stored value could
    /// not be read back as text (the row itself survives that degradation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout_json: Option<String>,
    /// The launch profile the window was opened from. `None` when it was
    /// opened from a raw argv, or when the daemon predates this field.
    /// Omitted from the wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The `model` launch option the window was opened with, as passed to
    /// the profile. `None` when none was given. Omitted from the wire when
    /// `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The `effort` launch option the window was opened with, as passed to
    /// the profile. `None` when none was given. Omitted from the wire when
    /// `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// Upper bound (bytes) on one window's layout_json, shared by the daemon's
/// store enforcement and the GUI-side pre-send validation.
pub const MAX_LAYOUT_JSON_BYTES: usize = 16_384;

/// One value of a profile as the daemon uses it, with the layer it comes
/// from. Carried by [`DaemonMessage::ProfileDetail`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileDetailEntry {
    /// The key path, outermost table first (`["model", "aliases", "fast"]`).
    pub path: Vec<String>,
    /// The value in use. A table is never a value: its keys are entries of
    /// their own, while an array is one value.
    pub value: serde_json::Value,
    /// The shipped profile's value at this path. Absent when the shipped
    /// profile has none, or when there is no shipped profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipped: Option<serde_json::Value>,
    /// Whether the user's own file sets this value.
    pub changed: bool,
}

/// The argv a profile launches on the daemon's host, with the layer it comes
/// from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileCommandDetail {
    /// The unresolved argv in use, as [`ProfileInfo::command`] carries it.
    pub argv: Vec<String>,
    /// The shipped profile's argv on this host. Absent when there is no
    /// shipped profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipped: Option<Vec<String>>,
    /// Whether the user's own file has a `[command]` table.
    pub changed: bool,
}

/// One launch profile, as reported by [`ClientMessage::ListProfiles`].
///
/// A 1:1 wire mirror of `yagura_daemon::profile::ProfileEntry`: `cwd` is
/// carried as a `String` here rather than the daemon's `PathBuf`, which has
/// no portable wire representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileInfo {
    /// The profile identifier: the TOML file's name stem.
    pub id: String,
    /// The profile's declared display name.
    pub display_name: String,
    /// The detection definition this profile names, if any. `None` selects
    /// the daemon's configured default.
    pub detection: Option<String>,
    /// English reason text when this entry's detection cannot be counted on:
    /// the name breaks the naming rule, no definition layer is configured at
    /// all, the name is absent from an inventory that was read, or that
    /// inventory could not be read. `None` when the name is present in the
    /// inventory, and when `detection` is `None` unless no layer is configured
    /// at all.
    pub detection_warning: Option<String>,
    /// The profile's own default working directory, if it declared one.
    pub cwd: Option<String>,
    /// The unresolved argv a launch of this profile would carry, with the
    /// operating-system selection already applied. Empty when the profile
    /// omits `[command]` while no default shell was resolved. Carried even when
    /// empty, so the key is always present on the wire.
    pub command: Vec<String>,
    /// Whether the profile currently resolves to something launchable.
    /// `true` does not guarantee a launch will succeed.
    pub available: bool,
    /// English reason text when `available` is `false`. `None` when
    /// `available` is `true`.
    pub unavailable_reason: Option<String>,
    /// The `model` launch option this profile maps, `None` (`null` on the
    /// wire) when the profile accepts no `model`. Absent from a frame written
    /// before the field existed, which decodes as `None`.
    #[serde(default)]
    pub model: Option<LaunchOptionInfo>,
    /// The `effort` launch option this profile maps, with the same absence
    /// semantics as `model`.
    #[serde(default)]
    pub effort: Option<LaunchOptionInfo>,
    /// Whether the user lets this profile be launched. Always written.
    ///
    /// Absence decodes as `true`, an exception to R2: a daemon without this
    /// field has no disabled state and every profile it lists can be opened,
    /// so reading absence as enabled states a fact about that older peer.
    #[serde(default = "enabled_when_absent")]
    pub enabled: bool,
}

fn enabled_when_absent() -> bool {
    true
}

/// One launch option a profile accepts, as [`ClientMessage::ListProfiles`]
/// reports it. The argv template the profile maps the value through is the
/// author's concern and is not carried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchOptionInfo {
    /// Alias -> value pairs in the profile file's order; empty when the
    /// profile maps the option but declares no alias.
    pub aliases: Vec<LaunchOptionAlias>,
}

/// One alias a launch option declares: the short name a caller may supply
/// and the value the profile turns it into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchOptionAlias {
    /// The name a caller supplies.
    pub alias: String,
    /// The value the alias stands for.
    pub value: String,
}

/// Where a profile's list of selectable models comes from, as
/// [`DaemonMessage::ModelChoices`] reports it.
///
/// Carries no catch-all variant: a `kind` outside this set fails to decode
/// rather than turning into a guessed source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelChoiceSource {
    /// The profile lists the models itself.
    Values,
    /// The daemon ran a command to list the models.
    Command {
        /// The command's argv as the profile declares it, before the daemon
        /// resolves the program.
        argv: Vec<String>,
    },
}

/// One model a caller may pick as an alias's value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoice {
    /// The value passed to the launch option.
    pub id: String,
    /// A display name for the model. Omitted from the wire (rather than sent
    /// as `null`) when the source offers none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// One pending approval request: delivered to every registered approver
/// connection when submitted, and to the client the settled decision
/// concerned.
///
/// `target.status` is a snapshot taken at submit time, not a live value: an
/// approver reasoning about it later may be looking at a window whose status
/// has since changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// This request's identifier, unique within this daemon boot.
    pub request_id: String,
    /// The gated operation's external name (`"open_window"`,
    /// `"close_window"`, `"interrupt_window"`, `"send_message"`,
    /// `"send_keys"`, `"send_handoff"`, `"send_chat"` or `"broadcast_chat"`).
    pub tool: String,
    /// Who asked.
    pub requester: ApprovalRequester,
    /// The window the operation would act on. `None` for a chat post
    /// (`send_chat` / `broadcast_chat`), which is not addressed to any one
    /// window, and for `open_window` and a `send_handoff` that opens a window
    /// from a profile, whose window does not exist yet; omitted from the wire
    /// (rather than sent as `null`) in those cases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ApprovalTarget>,
    /// The body a human must see before deciding: the `send_message` text,
    /// the `send_handoff` / chat envelope or, for `open_window`, the lines
    /// naming what would be started (`command:` or `profile:`, `cwd:` and,
    /// when known, `for:`); a `send_handoff` that opens a window carries
    /// those lines followed by its envelope. Omitted from the wire (rather than
    /// sent as `null`) for `close_window` and `interrupt_window`, which carry
    /// no body. The submit key and any paste framing the daemon adds when
    /// writing it are not part of this text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The chat room a post would go to. `Some` only for `send_chat` /
    /// `broadcast_chat`; omitted from the wire (rather than sent as `null`)
    /// for every other tool, so a client built before this field existed
    /// keeps decoding every request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<RoomRef>,
}

/// A room's identifier and the display name it resolved to against the room
/// list current at the moment this value was produced. Never a path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomRef {
    /// The room's identifier.
    pub room_id: String,
    /// The room's display name, resolved against the rooms that existed at
    /// this value's construction time.
    pub name: String,
}

/// What kind of party a pending [`ApprovalRequest`] came from.
///
/// An unrecognized wire value decodes as [`ApprovalRequesterKind::Unknown`]
/// rather than failing the frame, so a peer built against a newer protocol
/// that adds a kind stays interoperable. This build never sends `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRequesterKind {
    /// A client that has not claimed a window.
    External,
    /// A client that identified itself as a window.
    Window,
    /// A kind this build does not know.
    #[serde(other)]
    Unknown,
}

/// Who a pending [`ApprovalRequest`] came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequester {
    /// Which kind of party asked.
    pub kind: ApprovalRequesterKind,
    /// The requesting window's id. `Some` only for
    /// [`ApprovalRequesterKind::Window`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    /// The requesting window's display name. `Some` only for
    /// [`ApprovalRequesterKind::Window`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The window a pending [`ApprovalRequest`] would act on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalTarget {
    /// The target window's id.
    pub window_id: String,
    /// The target window's display name.
    pub name: String,
    /// The target window's status at the moment the request was submitted.
    pub status: WindowStatus,
}

/// Why a pending [`ApprovalRequest`] ended without a decision from the
/// approver being told about it.
///
/// An unrecognized wire value decodes as [`WithdrawReason::Unknown`] rather
/// than failing the frame, so a peer built against a newer protocol that adds
/// a reason stays interoperable. This build never sends `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WithdrawReason {
    /// Another approver decided first.
    Resolved,
    /// The approval deadline elapsed.
    Timeout,
    /// The requester's connection closed.
    RequesterDisconnected,
    /// A reason this build does not know.
    #[serde(other)]
    Unknown,
}

/// What became of a handoff envelope: the receiving CLI took it, or it never
/// will.
///
/// The daemon does not type an envelope into a window's composer to watch what
/// the composer does with it: a window whose profile declares how text reaches
/// its CLI is handed the envelope through that CLI's own doors, or as the
/// launch prompt of a window the handoff opens.
///
/// # Examples
///
/// ```
/// use yagura_protocol::HandoffDelivery;
///
/// let encoded = serde_json::to_string(&HandoffDelivery::Taken)
///     .expect("a plain enum always encodes");
/// assert_eq!(encoded, "\"taken\"");
/// assert!(serde_json::from_str::<HandoffDelivery>("\"bogus\"").is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffDelivery {
    /// The receiving CLI took the envelope: one of its doors returned it, the
    /// daemon handed it over through the CLI's own command or typed it and
    /// pressed the submit key, or the window this handoff opened was started
    /// with it as its launch prompt.
    Taken,
    /// Nothing took the envelope within the daemon's limit, or the request
    /// was withdrawn first; it was taken out of the window's inbox before
    /// this answer, so it never arrives later.
    UndeliveredTimeout,
    /// The window declares no way to deliver it: it was not opened from a
    /// profile that declares delivery, or its hooks were left out at launch.
    UndeliveredNoDelivery,
}

/// A 24-bit color as `[red, green, blue]`.
pub type Rgb = [u8; 3];

/// A color of a cell as the terminal model holds it. The GUI resolves it
/// against its theme; named colors 0-15 arrive as `Indexed`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum CellColor {
    /// The theme's default foreground or background.
    #[default]
    Default,
    /// A palette entry, 0-255 (0-15 are what SGR 30-37/90-97/40-47/100-107 name).
    Indexed(u8),
    /// A 24-bit color.
    Rgb(Rgb),
}

/// A flag of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellFlag {
    /// SGR 1.
    Bold,
    /// SGR 2.
    Dim,
    /// SGR 3.
    Italic,
    /// SGR 4 in any of its forms.
    Underline,
    /// SGR 9.
    Strikeout,
    /// SGR 7.
    Inverse,
    /// SGR 8.
    Hidden,
    /// The cell is inside the window's selection.
    Selected,
}

/// What a run of cells looks like.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CellAttr {
    /// Foreground.
    #[serde(default)]
    pub fg: CellColor,
    /// Background.
    #[serde(default)]
    pub bg: CellColor,
    /// Flags, in `CellFlag` declaration order.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub flags: BTreeSet<CellFlag>,
}

/// Consecutive cells sharing one attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRun {
    /// Index into [`Frame::attrs`].
    pub attr: u16,
    /// One string per cell: the base character plus any combining characters
    /// the model holds in that cell. A wide cell appears once; its spacer is
    /// not listed.
    pub cells: Vec<String>,
    /// Indices (into `cells`, ascending) of the cells that are two columns wide.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wide: Vec<u16>,
}

/// One screen row of a frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameLine {
    /// The row, from 0 at the top of the view.
    pub row: u16,
    /// The line goes on in the next row (the model wrapped it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wrapped: bool,
    /// The cells from column 0 on. Columns after the last run are blank cells
    /// with the default attribute.
    pub runs: Vec<FrameRun>,
}

/// The shape of the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    /// A filled block.
    Block,
    /// A line under the cell.
    Underline,
    /// A vertical bar before the cell.
    Bar,
    /// An outlined block.
    HollowBlock,
}

/// The cursor in the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorState {
    /// The row in the view.
    pub row: u16,
    /// The column; `cols` while a character waits to wrap.
    pub col: u16,
    /// Whether the program shows the cursor (`CSI ? 25 h`). The renderer draws
    /// no cursor while `ViewState::display_offset` is not 0.
    pub visible: bool,
    /// The shape the program asked for.
    pub shape: CursorShape,
}

/// What the view shows besides cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewState {
    /// How many rows the view is scrolled back from the live screen (0 = live).
    pub display_offset: u32,
    /// How many rows of history exist above the live screen.
    pub history_rows: u32,
    /// The program asked to receive mouse reports.
    pub mouse_reporting: bool,
    /// The alternate screen is up.
    pub alt_screen: bool,
}

/// A color slot a program can change with OSC 4/10/11/12.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaletteSlot {
    /// The default foreground (OSC 10).
    Foreground,
    /// The default background (OSC 11).
    Background,
    /// The cursor color (OSC 12).
    Cursor,
    /// A palette entry (OSC 4).
    Indexed(u8),
}

/// A color a program set over the theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColorOverride {
    /// The slot.
    pub slot: PaletteSlot,
    /// Its color.
    pub rgb: Rgb,
}

/// The theme colors the GUI draws with: [`ClientMessage::SetPalette`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Palette {
    /// Default foreground.
    pub foreground: Rgb,
    /// Default background.
    pub background: Rgb,
    /// Cursor color.
    pub cursor: Rgb,
    /// Colors 0-15.
    pub ansi: Vec<Rgb>,
}

/// One part of a frame: [`DaemonMessage::Frame`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    /// The frame number; every part of one frame carries the same value.
    pub seq: u64,
    /// Columns of the view.
    pub cols: u16,
    /// Rows of the view.
    pub rows: u16,
    /// Blank a `cols` x `rows` grid before applying `lines`. Only the first
    /// part of a frame can set it.
    pub reset: bool,
    /// This is the last part of frame `seq`; only it is acknowledged.
    pub last: bool,
    /// The attributes `lines` refer to.
    pub attrs: Vec<CellAttr>,
    /// The rows this part replaces.
    pub lines: Vec<FrameLine>,
    /// The cursor.
    pub cursor: CursorState,
    /// The view's state.
    pub view: ViewState,
    /// Every color the program has set over the theme (the whole table).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<ColorOverride>,
}

/// A terminal size in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    /// Number of columns.
    pub cols: u16,
    /// Number of rows.
    pub rows: u16,
}

/// A message sent from the client (GUI) to the daemon.
///
/// Serialized with an internal tag (`"type"`, `snake_case`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Request to attach to a specific window session, at the size the caller
    /// will draw it at. The daemon resizes the window to `size` (when given
    /// and different), answers with [`DaemonMessage::Attached`] and then
    /// sends a [`DaemonMessage::Frame`] of every row, whatever frame is
    /// unacknowledged or held. Attaching again starts the view over.
    AttachView {
        /// The window ID to attach to (must be known to the daemon).
        window_id: String,
        /// The size to draw at, within [`MIN_WINDOW_COLS`]`..=`
        /// [`MAX_WINDOW_DIMENSION`] columns and [`MIN_WINDOW_ROWS`]`..=`
        /// [`MAX_WINDOW_DIMENSION`] rows; outside it the daemon refuses with
        /// `invalid_argument` and changes nothing. `None` keeps the window's
        /// size. A window whose child has exited keeps its size either way.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<TerminalSize>,
    },
    /// The client applied the last part of frame `seq`. The daemon sends no
    /// frame while one is unacknowledged.
    FrameAck {
        /// The frame number of the acknowledged frame.
        seq: u64,
    },
    /// The theme colors the client draws with. The daemon answers the
    /// attached window's color queries (OSC 4/10/11/12) with them; before it
    /// receives one it leaves those queries unanswered.
    SetPalette(Palette),
    /// Whether the client shows the attached window. While it does not, the
    /// daemon builds no frames for it; once it does, the next frame carries
    /// every row. A view starts shown.
    SetVisible {
        /// `true` when the window is on screen.
        visible: bool,
    },
    /// Asks for the attached window's selected text, answered by
    /// [`DaemonMessage::SelectionText`].
    CopySelection,
    /// Clears the attached window's selection; the next frame no longer marks
    /// its cells. Nothing is answered.
    ClearSelection,
    /// Moves the attached window's view `lines` rows; positive goes back into
    /// the history. The daemon keeps the view within the history.
    ScrollView {
        /// Rows to move.
        lines: i32,
    },
    /// A key press for the attached window. The daemon writes the bytes the
    /// window's terminal modes call for, or nothing.
    Key(KeyInput),
    /// Committed text (typed or from an IME) for the attached window, at most
    /// [`MAX_TEXT_INPUT_BYTES`] bytes.
    Text {
        /// The text, written as UTF-8 unchanged.
        text: String,
    },
    /// A paste into the attached window, at most [`MAX_PASTE_BYTES`] bytes.
    /// The daemon turns line breaks into CR and brackets it when the window
    /// asked for bracketed paste.
    Paste {
        /// The pasted text.
        text: String,
    },
    /// A pointer event in the attached window. Reported to the child when it
    /// tracks the mouse; otherwise not written.
    Pointer(PointerInput),
    /// A wheel event in the attached window.
    Wheel(WheelInput),
    /// The attached window gained or lost the focus. Reported to the child
    /// when it asked for focus reports.
    Focus {
        /// `true` when the window gained the focus.
        focused: bool,
    },
    /// Request to resize the PTY.
    Resize {
        /// New column count, within
        /// [`MIN_WINDOW_COLS`]`..=`[`MAX_WINDOW_DIMENSION`] (checked by the
        /// daemon, not this crate).
        cols: u16,
        /// New row count, within
        /// [`MIN_WINDOW_ROWS`]`..=`[`MAX_WINDOW_DIMENSION`] (checked the same
        /// way).
        rows: u16,
    },
    /// Liveness check; the daemon replies with [`DaemonMessage::Pong`].
    Ping,
    /// Request the daemon to terminate the child process tree and exit.
    Shutdown,
    /// Report the current or desired status of the child process (structured source).
    StatusEvent {
        /// Identifier of the process or source reporting the status (e.g., `"cli"`,
        /// `"claude-code"`, `"hook_source"`). Must match `[a-z0-9_-]{1,64}`.
        source: String,
        /// The reported status.
        status: WindowStatus,
        /// Target window ID. The daemon refuses an event without one as
        /// `invalid_argument`; it stays optional on decode so that such an
        /// event is answered with that error instead of failing the frame.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window_id: Option<String>,
        /// Per-boot authentication token. Required: the pre-token
        /// wire form is rejected outright (fail-closed) rather than decoded
        /// into an unauthenticated event.
        token: String,
    },
    /// Open a new window with the given command and optional configuration.
    OpenWindow {
        /// Command and arguments to execute (required, non-empty).
        command: Vec<String>,
        /// Optional working directory (absolute or resolved by daemon).
        cwd: Option<String>,
        /// Optional display name for the window.
        name: Option<String>,
        /// Optional vendor identifier. Displayed beside the name, so it must
        /// satisfy [`validate_window_vendor`].
        vendor: Option<String>,
        /// Optional detection definition name (defaults to daemon's configured default).
        detection: Option<String>,
    },
    /// List all windows currently managed by the daemon.
    ListWindows,
    /// Query the status of a specific window.
    GetWindowStatus {
        /// The window ID to query (must be known to the daemon).
        window_id: String,
    },
    /// Send text input to a specific window without requiring an active attachment.
    SendText {
        /// The window ID to send text to.
        window_id: String,
        /// The text to send (bytes are treated as UTF-8 or raw binary as
        /// appropriate).
        ///
        /// A payload holding a single line -- one trailing `\r`, `\n` or
        /// `\r\n` terminator, no other CR or LF, and a non-empty body before
        /// it -- is written through the target window's detection definition
        /// `[input]` plan: the body and the submit key are written as
        /// separate steps, and the trailing terminator is replaced by that
        /// definition's submit key. Every other payload, multi-line or
        /// unterminated, is written through unchanged.
        text: String,
    },
    /// Send a closed set of named control keys to a specific window.
    SendKeys {
        /// The window ID to send keys to.
        window_id: String,
        /// Named keys; the daemon validates the sequence before approval.
        keys: Vec<ControlKey>,
    },
    /// Request to interrupt the child process (write the configured interrupt
    /// byte sequence to the PTY).
    InterruptWindow {
        /// The window ID to interrupt.
        window_id: String,
    },
    /// Request to close the window and terminate its process tree.
    CloseWindow {
        /// The window ID to close.
        window_id: String,
        /// When `true`, skip the graceful termination grace period and
        /// terminate the Job Object immediately. Defaults to `false` when
        /// omitted on the wire.
        force: Option<bool>,
    },
    /// Wait until the window reaches (or is confirmed to not be able to
    /// reach) the given status, or until the timeout elapses.
    WaitForStatus {
        /// The window ID to wait on.
        window_id: String,
        /// The status to wait for. `Unknown` is a well-formed value at the
        /// protocol layer; rejecting it as a wait target is an IPC-layer
        /// concern.
        target_status: WindowStatus,
        /// Maximum time to wait, in seconds.
        timeout_sec: u64,
    },
    /// Change a window's display name. A label change only;
    /// it never touches the child process, so an exited window can still be
    /// renamed.
    RenameWindow {
        /// The window ID to rename.
        window_id: String,
        /// The requested display name. Must satisfy [`validate_window_name`];
        /// every rejection reason collapses into one `invalid_argument`
        /// error on the wire.
        name: String,
    },
    /// Read the tail of the window's text as the window shows it, in logical
    /// lines (a row the window wrapped joins the next).
    ReadOutput {
        /// The window ID to read output from.
        window_id: String,
        /// Number of trailing lines to return. When omitted, the daemon
        /// applies its default (100).
        lines: Option<u32>,
    },
    /// Requests the list of detection definition names the daemon can serve:
    /// the union of its bundled definitions and its resolved detection
    /// directory, where a directory definition hides a bundled one of the
    /// same name.
    ListDetections,
    /// Requests the previous boot's window configuration (prev slot) for the
    /// restore UI. Read-only: never rotates or writes the store.
    ListRestoreCandidates,
    /// Reports a window's presentation layout as an opaque JSON string the GUI
    /// owns. The daemon stores it verbatim (bounded by a byte limit the IPC
    /// layer enforces) and never interprets it.
    UpdateWindowLayout {
        /// The window ID whose layout is being reported.
        window_id: String,
        /// The layout, as a JSON string whose schema belongs to the client.
        layout_json: String,
    },
    /// Requests the list of launch profiles found under the daemon's
    /// resolved profiles directory. Always answered, even when no directory
    /// is configured (an empty list, with a warning explaining why).
    ListProfiles,
    /// Opens a new window from a launch profile rather than an explicit
    /// command. Mutually exclusive with an explicit command at the caller's
    /// entry point (the Tauri command / MCP tool reject the combination);
    /// this message itself carries no command field to conflict with.
    OpenWindowFromProfile {
        /// The profile identifier: a TOML file's name stem under the
        /// resolved profiles directory.
        profile: String,
        /// Optional working directory, overriding the profile's own
        /// default.
        cwd: Option<String>,
        /// Optional display name, overriding the profile's `display_name`.
        name: Option<String>,
        /// Optional `model` launch option, mapped through the profile.
        /// Defaulted so a frame written before the field existed decodes.
        #[serde(default)]
        model: Option<String>,
        /// Optional `effort` launch option, mapped through the profile.
        #[serde(default)]
        effort: Option<String>,
    },
    /// Claims this connection's origin as a specific window, proven by that
    /// window's per-boot status token. Distinct connections identify
    /// independently, and the outcome only ever widens what the connection
    /// may act on without a human in the loop (`External -> Window(id)`).
    Identify {
        /// The window ID claiming this connection.
        window_id: String,
        /// The window's per-boot status token.
        token: String,
    },
    /// Claims this connection as an approver: a party allowed to decide every
    /// pending [`ApprovalRequest`], and to act on any window without going
    /// through the approval gate itself.
    RegisterApprover,
    /// Hands work to another window: an existing one (`window_id`) or a new
    /// one opened from a launch profile (`profile`). Exactly one of the two,
    /// as [`validate_handoff_target`] decides.
    SendHandoff {
        /// The window to hand the work to. Mutually exclusive with
        /// `profile`.
        window_id: Option<String>,
        /// The launch profile to open a new window from. Mutually exclusive
        /// with `window_id`.
        profile: Option<String>,
        /// Only with `profile`; overrides the requester's cwd / the profile
        /// default.
        cwd: Option<String>,
        /// Only with `profile`; overrides the profile's `display_name`.
        name: Option<String>,
        /// Only with `profile`; the `model` launch option mapped through it.
        /// Defaulted so a frame written before the field existed decodes.
        #[serde(default)]
        model: Option<String>,
        /// Only with `profile`; the `effort` launch option mapped through it.
        #[serde(default)]
        effort: Option<String>,
        /// What to look at (path, git range, URL...). Opaque to the daemon,
        /// bounded by [`HANDOFF_REFERENCE_MAX_BYTES`].
        reference: String,
        /// The requester's own words. Opaque to the daemon, bounded by
        /// [`HANDOFF_NOTE_MAX_BYTES`].
        note: String,
    },
    /// Settles one pending [`ApprovalRequest`]. Only accepted from a
    /// connection that has sent `register_approver`.
    ApprovalDecision {
        /// The request being settled.
        request_id: String,
        /// `true` to approve, `false` to deny.
        approved: bool,
    },
    /// Posts a message to one chat room, addressed to the window(s) its `@`
    /// mentions resolve to (or nobody, if it names none).
    SendChat {
        /// The room to post to. `None` selects the caller's own workspace
        /// room, and `"main"` the room every caller may reach; which other
        /// rooms a caller may name is the daemon's decision.
        room_id: Option<String>,
        /// The message body, at most [`CHAT_BODY_MAX_BYTES`] bytes.
        body: String,
    },
    /// Posts a message to every participant in the room but the poster.
    BroadcastChat {
        /// Same as [`ClientMessage::SendChat::room_id`].
        room_id: Option<String>,
        /// Same as [`ClientMessage::SendChat::body`].
        body: String,
    },
    /// Reads a page of the room's message history.
    ReadChat {
        /// Same as [`ClientMessage::SendChat::room_id`].
        room_id: Option<String>,
        /// Return only messages posted after this sequence number. See
        /// [`DaemonMessage::ChatMessages::next_after_seq`] for how to page
        /// forward, and the daemon's own rule for what an omitted value
        /// defaults to per caller kind.
        after_seq: Option<u64>,
        /// Maximum number of messages to return, at most
        /// [`CHAT_READ_LIMIT_MAX`]. Defaults to 50 when omitted.
        limit: Option<u32>,
    },
    /// Requests every chat room this daemon knows of. Approver-only: the
    /// listing names the workspaces windows are open in, which no other
    /// kind of connection is told about.
    ListChatRooms,
    /// Asks which wire revision the daemon implements; answered with
    /// [`DaemonMessage::ProtocolRevision`]. A daemon built before this type
    /// existed refuses it by name, and that refusal is itself an answer: such
    /// a peer is credited with [`PROTOCOL_REVISION_BASELINE`].
    GetProtocolRevision,
    /// Asks what keeps this daemon from doing its job; answered with
    /// [`DaemonMessage::DaemonHealth`]. Needs no permission: the answer names
    /// no window, path or user data.
    GetDaemonHealth,
    /// A CLI's start-up hook asks for the window's start-up context: the
    /// introduction plus whatever its inbox holds; answered with
    /// [`DaemonMessage::Delivery`]. Authenticated like a status event.
    DeliverContext {
        /// The window the hook runs in.
        window_id: String,
        /// That window's own token.
        token: String,
    },
    /// A CLI's turn-start hook takes the parcels waiting in the window's
    /// inbox; answered with [`DaemonMessage::Delivery`].
    DeliverTake {
        /// The window the hook runs in.
        window_id: String,
        /// That window's own token.
        token: String,
    },
    /// An idle window's hook waits for a parcel. The daemon answers with
    /// [`DaemonMessage::Delivery`] when one arrives, or with an empty one
    /// when a newer wait for the same window replaces this one. The wait ends
    /// when the connection closes.
    DeliverWait {
        /// The window the hook runs in.
        window_id: String,
        /// That window's own token.
        token: String,
    },
    /// A CLI's hook tells the daemon its session identifier; answered with
    /// [`DaemonMessage::SessionRecorded`].
    ReportSession {
        /// The window the hook runs in.
        window_id: String,
        /// That window's own token.
        token: String,
        /// The identifier the hook read from the CLI's input.
        session: String,
    },
    /// Records a new plan owned by the calling window; answered with
    /// [`DaemonMessage::PlanDetail`].
    CreatePlan {
        /// The plan's display title.
        title: String,
        /// Every task of the plan, in the caller's order.
        tasks: Vec<PlanTaskSpec>,
    },
    /// Replaces every task of an existing plan (and optionally its title);
    /// answered with [`DaemonMessage::PlanDetail`].
    UpdatePlan {
        /// The plan to replace.
        plan_id: String,
        /// The new title. `None` keeps the stored one.
        title: Option<String>,
        /// The full replacement task list, not a delta.
        tasks: Vec<PlanTaskSpec>,
    },
    /// Reads one plan; answered with [`DaemonMessage::PlanDetail`].
    GetPlan {
        /// The plan to read.
        plan_id: String,
    },
    /// Lists the plans the caller may see; answered with
    /// [`DaemonMessage::PlanList`].
    ListPlans,
    /// Deletes one plan; answered with [`DaemonMessage::PlanDeleted`].
    DeletePlan {
        /// The plan to delete.
        plan_id: String,
    },
    /// Reads the role-to-assignment table; answered with
    /// [`DaemonMessage::RoleList`].
    ListRoles,
    /// Replaces the whole role-to-assignment table; answered with
    /// [`DaemonMessage::RoleList`].
    SaveRoles {
        /// The full replacement table.
        roles: Vec<RoleEntry>,
        /// The role name to fall back on for a task that names no role, or
        /// names one the table has no row for. `None` clears it. Defaulted
        /// so a frame written before the field existed decodes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_role: Option<String>,
    },
    /// Sets the user's assignment override for one task; answered with
    /// [`DaemonMessage::PlanDetail`].
    SetTaskAssignment {
        /// The plan holding the task.
        plan_id: String,
        /// The task to override.
        task_id: String,
        /// The assignment that takes precedence over every other source.
        assignment: TaskAssignment,
    },
    /// Removes the user's assignment override from one task; answered with
    /// [`DaemonMessage::PlanDetail`].
    ClearTaskAssignment {
        /// The plan holding the task.
        plan_id: String,
        /// The task whose override is removed.
        task_id: String,
    },
    /// Names a window as a plan's owner; answered with
    /// [`DaemonMessage::PlanDetail`].
    SetPlanOwner {
        /// The plan whose owner changes.
        plan_id: String,
        /// The window that becomes the owner.
        window_id: String,
    },
    /// Opens a window for one task with the task's resolved assignment;
    /// answered with [`DaemonMessage::TaskWindowOpened`].
    OpenTaskWindow {
        /// The plan holding the task.
        plan_id: String,
        /// The task to open a window for.
        task_id: String,
        /// Optional display name for the new window.
        name: Option<String>,
    },
    /// Settles a [`PlanReview::Pending`] plan to [`PlanReview::Approved`],
    /// provided `version` matches the plan's current
    /// [`PlanInfo::version`]; answered with [`DaemonMessage::PlanDetail`].
    /// Approver-only, like every other message this variant's doc links to.
    ApprovePlan {
        /// The plan to approve.
        plan_id: String,
        /// The version the caller last read, so a concurrent
        /// `replace_tasks` between read and approve is rejected rather than
        /// approving tasks the caller never saw.
        version: u64,
    },
    /// Clears every [`PlanTaskInfo::review_mark`] of the plan, provided
    /// `version` matches its current [`PlanInfo::version`]; answered with
    /// [`DaemonMessage::PlanDetail`]. Approver-only.
    MarkPlanSeen {
        /// The plan whose marks are cleared.
        plan_id: String,
        /// Same role as [`ClientMessage::ApprovePlan::version`].
        version: u64,
    },
    /// Reports how the work handed to the calling window ended; answered
    /// with [`DaemonMessage::ReportSent`].
    SendReport {
        /// Whether the work succeeded.
        outcome: ReportOutcome,
        /// The reporting window's account, bounded by
        /// [`REPORT_SUMMARY_MAX_BYTES`].
        summary: String,
        /// Where a longer result was written, bounded by
        /// [`REPORT_PATH_MAX_BYTES`].
        path: Option<String>,
    },
    /// Takes every report waiting for the calling window, waiting up to
    /// `timeout_sec` for one when none is waiting; answered with
    /// [`DaemonMessage::Reports`].
    WaitForReport {
        /// How long to wait for a report, in seconds. `0` returns at once.
        timeout_sec: u64,
    },
    /// Asks for the models a profile's `model` launch option can take;
    /// answered at once with [`DaemonMessage::ModelChoicesRequested`], and
    /// later with [`DaemonMessage::ModelChoices`] carrying the list.
    ListModelChoices {
        /// The profile whose models are listed.
        profile: String,
        /// Chosen by the requester; the daemon copies it verbatim into both
        /// the immediate answer and the later push, so the requester can pair
        /// them regardless of the order they arrive in.
        request_id: u64,
    },
    /// Replaces the aliases of a profile's `model` launch option; answered
    /// with [`DaemonMessage::ModelAliasesSaved`]. Approver-only.
    SaveModelAliases {
        /// The profile whose aliases are replaced.
        profile: String,
        /// The aliases to store, in the order to store them.
        aliases: Vec<LaunchOptionAlias>,
    },
    /// Turns a profile's launching on or off; answered with
    /// [`DaemonMessage::ProfileEnabledSet`]. Approver-only.
    SetProfileEnabled {
        /// The profile to turn on or off.
        profile: String,
        /// `true` lets the profile be launched, `false` refuses its launches.
        enabled: bool,
    },
    /// Reads every value of a profile as the daemon uses it, with the layer
    /// each comes from; answered with [`DaemonMessage::ProfileDetail`].
    /// Approver-only: the values include the profile's environment.
    GetProfileDetail {
        /// The profile to read.
        profile: String,
    },
    /// Sets the argv a profile launches on the daemon's host, or puts the
    /// shipped one back; answered with [`DaemonMessage::ProfileCommandSet`].
    /// Approver-only.
    SetProfileCommand {
        /// The profile to change.
        profile: String,
        /// The argv to launch, or `None` to remove the user's own
        /// `[command]` and use the shipped one.
        argv: Option<Vec<String>>,
    },
    /// Reads the daemon's current approval settings; answered with
    /// [`DaemonMessage::ApprovalSettings`]. Approver-only.
    GetApprovalSettings {},
    /// Turns automatic approval of one destructive tool on or off; answered
    /// with [`DaemonMessage::AutoApproveSet`]. Approver-only.
    SetAutoApprove {
        /// The tool to change (an unrecognized name is refused by the
        /// daemon, not by this crate).
        tool: String,
        /// `true` lets the tool proceed without approval, `false` requires
        /// it.
        auto_approve: bool,
    },
    /// Turns the plan review gate on or off; answered with
    /// [`DaemonMessage::PlanApprovalRequiredSet`]. Approver-only.
    SetPlanApprovalRequired {
        /// `true` requires review before a new plan is created, `false`
        /// skips it.
        required: bool,
    },
    /// Lists the profiles that describe how to register the MCP bridge with
    /// their CLI; answered with [`DaemonMessage::McpRegistrationsListed`],
    /// followed by one [`DaemonMessage::McpRegistrationStatus`] push per row.
    /// Approver-only.
    ListMcpRegistrations {
        /// Echoed by the answer and every push it causes.
        request_id: u64,
    },
    /// Registers, updates or removes the MCP bridge with one profile's CLI;
    /// answered with [`DaemonMessage::McpRegistrationRequested`], followed by
    /// one [`DaemonMessage::McpRegistrationResult`] push. Approver-only.
    RunMcpRegistration {
        /// The profile whose registration is changed.
        profile: String,
        /// What to do.
        action: McpRegistrationAction,
        /// Echoed by the answer and the push it causes.
        request_id: u64,
    },
}

/// The wire revision a peer must implement before `msg` may be sent to it.
///
/// The answer is derived from the message alone, so a caller never supplies
/// it, cannot forget it at one call site, and no second copy of the rule can
/// drift from this one. [`PROTOCOL_REVISION_BASELINE`] means "no
/// constraint": every peer that speaks this wire at all can take the message.
///
/// # Examples
///
/// ```
/// use yagura_protocol::{
///     ClientMessage, LAUNCH_OPTIONS_MIN_REVISION, PROTOCOL_REVISION_BASELINE,
///     required_protocol_revision,
/// };
///
/// let plain = ClientMessage::OpenWindowFromProfile {
///     profile: "claude".to_string(),
///     cwd: None,
///     name: None,
///     model: None,
///     effort: None,
/// };
/// assert_eq!(required_protocol_revision(&plain), PROTOCOL_REVISION_BASELINE);
///
/// let with_model = ClientMessage::OpenWindowFromProfile {
///     profile: "claude".to_string(),
///     cwd: None,
///     name: None,
///     model: Some("opus".to_string()),
///     effort: None,
/// };
/// assert_eq!(required_protocol_revision(&with_model), LAUNCH_OPTIONS_MIN_REVISION);
/// ```
pub fn required_protocol_revision(msg: &ClientMessage) -> u32 {
    match msg {
        ClientMessage::AttachView { .. }
        | ClientMessage::FrameAck { .. }
        | ClientMessage::SetPalette(_)
        | ClientMessage::SetVisible { .. }
        | ClientMessage::CopySelection
        | ClientMessage::ClearSelection
        | ClientMessage::ScrollView { .. } => FRAME_MIN_REVISION,
        ClientMessage::Key(_)
        | ClientMessage::Text { .. }
        | ClientMessage::Paste { .. }
        | ClientMessage::Pointer(_)
        | ClientMessage::Wheel(_)
        | ClientMessage::Focus { .. } => STRUCTURED_INPUT_MIN_REVISION,
        ClientMessage::SendKeys { .. } => SEND_KEYS_MIN_REVISION,
        ClientMessage::OpenWindowFromProfile { model, effort, .. }
        | ClientMessage::SendHandoff { model, effort, .. }
            if model.is_some() || effort.is_some() =>
        {
            LAUNCH_OPTIONS_MIN_REVISION
        }
        ClientMessage::CreatePlan { tasks, .. } | ClientMessage::UpdatePlan { tasks, .. }
            if tasks.iter().any(|task| task.instruction.is_some()) =>
        {
            TASK_INSTRUCTION_MIN_REVISION
        }
        ClientMessage::ApprovePlan { .. } | ClientMessage::MarkPlanSeen { .. } => {
            PLAN_REVIEW_MIN_REVISION
        }
        ClientMessage::SaveRoles {
            default_role: Some(_),
            ..
        } => PLAN_REVIEW_MIN_REVISION,
        ClientMessage::ListModelChoices { .. } | ClientMessage::SaveModelAliases { .. } => {
            MODEL_ALIASES_MIN_REVISION
        }
        ClientMessage::SetProfileEnabled { .. } => PROFILE_ENABLED_MIN_REVISION,
        ClientMessage::GetProfileDetail { .. } | ClientMessage::SetProfileCommand { .. } => {
            PROFILE_DETAIL_MIN_REVISION
        }
        ClientMessage::GetApprovalSettings { .. }
        | ClientMessage::SetAutoApprove { .. }
        | ClientMessage::SetPlanApprovalRequired { .. } => APPROVAL_SETTINGS_MIN_REVISION,
        ClientMessage::ListMcpRegistrations { .. } | ClientMessage::RunMcpRegistration { .. } => {
            MCP_REGISTRATION_MIN_REVISION
        }
        ClientMessage::GetDaemonHealth => DAEMON_HEALTH_MIN_REVISION,
        ClientMessage::DeliverContext { .. }
        | ClientMessage::DeliverTake { .. }
        | ClientMessage::DeliverWait { .. }
        | ClientMessage::ReportSession { .. } => INBOX_MIN_REVISION,
        _ => PROTOCOL_REVISION_BASELINE,
    }
}

/// A message sent from the daemon to the client (GUI).
///
/// Serialized with an internal tag (`"type"`, `snake_case`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMessage {
    /// Response to [`ClientMessage::AttachView`]: the child process status.
    /// A [`DaemonMessage::Frame`] of every row follows it.
    Attached {
        /// Whether the child process has already exited.
        child_exited: bool,
        /// Exit code of the child process, if it has exited.
        exit_code: Option<i32>,
    },
    /// One part of a frame of the attached window's view.
    Frame(Frame),
    /// Response to [`ClientMessage::CopySelection`].
    SelectionText {
        /// The selected text; `None` when nothing is selected.
        text: Option<String>,
    },
    /// The child process has exited.
    Exit {
        /// Exit code of the child process (`-1` if it could not be determined).
        exit_code: i32,
    },
    /// Response to [`ClientMessage::Ping`] (no fields).
    Pong,
    /// An error response. The connection may or may not be closed depending
    /// on `code` .
    Error {
        /// Machine-readable error code (e.g. `"frame_too_large"`,
        /// `"invalid_json"`, `"unknown_type"`, `"invalid_argument"`,
        /// `"child_exited"`, `"superseded"`, `"internal"`, `"window_not_found"`,
        /// `"spawn_failed"`, `"resource_exhausted"`, `"unsupported"`).
        code: String,
        /// Human-readable, English message. Must never contain frame bodies
        /// or raw PTY data.
        message: String,
        /// The name of the request argument this rejection points at (for
        /// example `"cwd"`), when it points at one the caller itself sent.
        /// A name, never a value: this carries no path, file content or
        /// secret. `None` when no single argument is at fault, or when the
        /// daemon predates this field. Omitted from the wire when `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rejected_argument: Option<String>,
    },
    /// Status change notification: the detected or reported status has changed.
    ///
    /// Sent only to the attached client. Includes the window ID to identify
    /// which window's status changed.
    StatusChanged {
        /// The window ID whose status changed.
        window_id: String,
        /// The new (confirmed) status of the child process.
        status: WindowStatus,
        /// Why `status` is `Unknown`, if it is. Omitted from the wire when
        /// `None`, so a peer predating this field still decodes (and
        /// produces) a valid `StatusChanged`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unknown_reason: Option<UnknownReason>,
        /// The generation of the status pair this push reports, carrying the
        /// same meaning as [`WindowInfo::status_rev`]: the reader applies it
        /// only when it is at least as new as the generation it already
        /// holds. `None` when the daemon predates this field. Omitted from
        /// the wire when `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status_rev: Option<u64>,
    },
    /// Response to [`ClientMessage::OpenWindow`]: the window was opened successfully.
    WindowOpened {
        /// The assigned window ID for the newly opened window.
        window_id: String,
        /// English reason text when the window runs no detection definition:
        /// one failed to load, or no definition layer is configured at all
        /// (see the branch table on `load_definition_for`). The window is open
        /// and its status stays `Unknown`. `None` when the definition loaded,
        /// or the daemon predates this field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detection_warning: Option<String>,
    },
    /// Response to [`ClientMessage::ListWindows`]: list of all managed windows.
    WindowList {
        /// Array of window metadata.
        windows: Vec<WindowInfo>,
    },
    /// Response to [`ClientMessage::GetWindowStatus`]: status of a specific window.
    WindowStatus {
        /// The window ID being queried.
        window_id: String,
        /// Current status of the window.
        status: WindowStatus,
        /// Milliseconds since the last status change. Always present (non-null).
        since_ms: u64,
        /// The last characters of the window's text, as `read_output` reads it.
        /// `None` if the window shows no text or it is unavailable.
        last_output_summary: Option<String>,
        /// Exit code if the window has exited, `None` if still running.
        exit_code: Option<i32>,
        /// Why `status` is `Unknown`, if it is. Omitted from the wire when
        /// `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unknown_reason: Option<UnknownReason>,
        /// English reason text carried alongside `unknown_reason` when the
        /// window runs no detection definition: one failed to load, or no
        /// definition layer is configured at all. `None` when a definition
        /// loaded. Omitted from the wire when `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detection_warning: Option<String>,
        /// Whether an MCP bridge has identified itself as this window over a
        /// connection that is still open; see [`WindowInfo::mcp_connected`].
        /// Omitted from the wire when `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mcp_connected: Option<bool>,
    },
    /// Response to [`ClientMessage::SendText`]: the text was written to the
    /// target window's PTY. Distinct from a generic empty success so that
    /// callers waiting on a single round-trip response (e.g. the MCP bridge)
    /// can observe success.
    TextSent {
        /// The window ID the text was sent to.
        window_id: String,
    },
    /// Response to [`ClientMessage::SendKeys`]: all named keys were written
    /// to the target window's PTY in one operation.
    KeysSent {
        /// The window ID the keys were sent to.
        window_id: String,
    },
    /// Response to [`ClientMessage::InterruptWindow`]: the interrupt byte
    /// sequence was written to the target window's PTY.
    WindowInterrupted {
        /// The window ID that was interrupted.
        window_id: String,
    },
    /// Response to [`ClientMessage::CloseWindow`]: the window's process tree
    /// was terminated (idempotently, if the child had already exited).
    WindowClosed {
        /// The window ID that was closed.
        window_id: String,
    },
    /// Response to [`ClientMessage::WaitForStatus`]. A timeout is a normal
    /// (non-error) outcome: `status` always reflects the status observed at
    /// response-generation time and is not guaranteed to equal the wait
    /// target even when `outcome` is `"reached"`.
    WaitResult {
        /// The window ID that was waited on.
        window_id: String,
        /// Either `"reached"` or `"timeout"`.
        outcome: String,
        /// The status observed at the time this response was produced.
        status: WindowStatus,
    },
    /// Response to [`ClientMessage::RenameWindow`]: the window's display
    /// name was changed.
    WindowRenamed {
        /// The window ID that was renamed.
        window_id: String,
        /// The name as stored, re-read under the daemon's lock rather than
        /// echoed back from the request, so that concurrent renames converge
        /// on one last-writer-wins value.
        name: String,
    },
    /// Response to [`ClientMessage::ReadOutput`]: the tail of the window's
    /// text as the window shows it.
    WindowOutput {
        /// The window ID the output was read from.
        window_id: String,
        /// The text, its logical lines joined with `\n`.
        text: String,
        /// Whether the returned text was truncated (by line count or byte
        /// budget).
        truncated: bool,
    },
    /// The detection definition names available for `open_window`'s
    /// `detection` argument, sorted in byte order.
    DetectionList {
        /// The union of `bundled_names` and `directory_names`. Empty when
        /// neither layer contributes a definition: no detection directory is
        /// resolved (or it holds no valid definition, or could not be read)
        /// and the bundled layer is off.
        names: Vec<String>,
        /// The names the daemon's bundled definitions supply: a subset of
        /// `names`, empty when the bundled layer is off (an explicitly
        /// configured detection directory answers alone).
        #[serde(default)]
        bundled_names: Vec<String>,
        /// The names the resolved detection directory supplies: a subset of
        /// `names`. A name in both layers is served from the directory.
        #[serde(default)]
        directory_names: Vec<String>,
        /// English reason text when the resolved detection directory could not
        /// be enumerated (`read_dir` failure). `None` when enumeration
        /// succeeded (an empty `names` with `None` genuinely means "no
        /// definitions") or when no directory is resolved.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        warning: Option<String>,
    },
    /// Response to [`ClientMessage::ListRestoreCandidates`].
    RestoreCandidateList {
        /// The previous boot's windows, in ascending `position` order.
        /// Empty when no configuration was stored, when persistence is
        /// disabled, or when the response budget left no room.
        windows: Vec<RestoreCandidate>,
        /// When that configuration was recorded, in Unix epoch seconds.
        /// `None` when the store never recorded it or the recorded value
        /// could not be read back as an integer. Always present on the wire
        /// (as `null` when absent), so the field's shape does not change with
        /// its value.
        taken_at_epoch_s: Option<i64>,
    },
    /// Response to [`ClientMessage::UpdateWindowLayout`].
    WindowLayoutUpdated {
        /// The window ID whose layout was stored.
        window_id: String,
    },
    /// Response to [`ClientMessage::ListProfiles`].
    ProfileList {
        /// Every profile the resolved directory currently holds.
        profiles: Vec<ProfileInfo>,
        /// One line per excluded file, plus -- when nothing could be
        /// enumerated at all (no directory configured, a missing directory,
        /// or a path that is not a directory) -- one line explaining why.
        warnings: Vec<String>,
    },
    /// Response to [`ClientMessage::Identify`]: the connection's origin is
    /// now the named window.
    Identified {
        /// The window ID this connection identified as.
        window_id: String,
    },
    /// Response to [`ClientMessage::RegisterApprover`]: the connection is now
    /// an approver. `pending` is every request awaiting a decision at the
    /// moment of registration, so a reconnecting approver's queue is
    /// restored rather than left silently empty.
    ApproverRegistered {
        /// Every currently pending request.
        pending: Vec<ApprovalRequest>,
    },
    /// Sent to every registered approver when a destructive operation needs a
    /// human decision.
    ApprovalRequested {
        /// The request awaiting a decision.
        request: ApprovalRequest,
    },
    /// Sent to every approver other than the one who decided (or to all of
    /// them, when the request ends without a decision): the dialog is no
    /// longer actionable.
    ApprovalWithdrawn {
        /// The request that is no longer pending.
        request_id: String,
        /// Why the request ended.
        reason: WithdrawReason,
    },
    /// Response to [`ClientMessage::ApprovalDecision`]: the request was
    /// settled.
    ApprovalResolved {
        /// The request that was settled.
        request_id: String,
    },
    /// Response to [`ClientMessage::SendHandoff`]: the receiving CLI took the
    /// envelope, or it never will.
    HandoffSent {
        /// The window that received the envelope (the newly opened one when
        /// `opened` is `true`).
        window_id: String,
        /// Whether this handoff opened the target window from a profile.
        opened: bool,
        /// What became of the envelope.
        delivery: HandoffDelivery,
    },
    /// Response to [`ClientMessage::SendChat`] / [`ClientMessage::BroadcastChat`]:
    /// the message was recorded (and, unless the room is halted, handed to a
    /// delivery task for each of `targets`).
    ChatPosted {
        /// The room the message was posted to.
        room_id: String,
        /// The message's sequence number within the room.
        seq: u64,
        /// The resolved window ids this message was addressed to.
        targets: Vec<String>,
        /// Whether this post's `@user` mention (or its poster) counts as the
        /// human user.
        user: bool,
        /// Raw mention text that did not resolve to exactly one participant.
        unresolved: Vec<String>,
        /// Whether the room is halted as of this post (whether or not this
        /// post is what halted it).
        halted: bool,
    },
    /// Response to [`ClientMessage::ReadChat`].
    ChatMessages {
        /// The room read from.
        room_id: String,
        /// The messages found, ascending by `seq`.
        messages: Vec<ChatMessage>,
        /// The cursor to pass as `after_seq` on the next call.
        next_after_seq: u64,
        /// The room's current halt, if any.
        halted: Option<ChatHalt>,
        /// Whether the room currently needs a human's attention.
        needs_user: bool,
        /// The room's current participants (never includes the user).
        roster: Vec<ChatParticipant>,
    },
    /// Sent to every registered approver connection when the room's
    /// `needs_user` state changes to `true`.
    ChatChanged {
        /// The room whose state changed.
        room_id: String,
        /// The sequence number of the message that triggered the change.
        seq: u64,
        /// The room's `needs_user` state at the moment of this push.
        needs_user: bool,
    },
    /// Response to [`ClientMessage::ListChatRooms`].
    ChatRooms {
        /// Every room the daemon knows of: `"main"` first, then the rest by
        /// most recent activity.
        rooms: Vec<ChatRoomInfo>,
    },
    /// Response to [`ClientMessage::GetProtocolRevision`].
    ProtocolRevision {
        /// The wire revision the answering daemon implements -- its own
        /// build's [`PROTOCOL_REVISION`], which is not necessarily the
        /// caller's.
        revision: u32,
    },
    /// Response to [`ClientMessage::GetDaemonHealth`].
    DaemonHealth {
        /// Every problem the daemon has right now; empty when it has none.
        problems: Vec<DaemonProblem>,
    },
    /// Response to [`ClientMessage::DeliverContext`],
    /// [`ClientMessage::DeliverTake`] and [`ClientMessage::DeliverWait`]: what
    /// the hook prints and how it ends.
    Delivery {
        /// The text for the hook to print; empty when there is nothing to
        /// say.
        output: String,
        /// The stream the hook prints `output` on.
        stream: DeliveryStream,
        /// The exit code the hook ends with.
        exit_code: u8,
    },
    /// Response to [`ClientMessage::ReportSession`].
    SessionRecorded {
        /// Whether the daemon now holds the reported identifier. `false` for
        /// a value of a shape the daemon does not keep.
        held: bool,
    },
    /// Response carrying one whole plan: to [`ClientMessage::CreatePlan`],
    /// [`ClientMessage::UpdatePlan`], [`ClientMessage::GetPlan`],
    /// [`ClientMessage::SetTaskAssignment`],
    /// [`ClientMessage::ClearTaskAssignment`] and
    /// [`ClientMessage::SetPlanOwner`].
    PlanDetail {
        /// The plan as stored after the request.
        plan: PlanInfo,
    },
    /// Response to [`ClientMessage::ListPlans`].
    PlanList {
        /// Every plan the caller may see.
        plans: Vec<PlanSummary>,
    },
    /// Response to [`ClientMessage::DeletePlan`].
    PlanDeleted {
        /// The plan that was deleted.
        plan_id: String,
    },
    /// Response to [`ClientMessage::ListRoles`] and
    /// [`ClientMessage::SaveRoles`]: the table as stored.
    RoleList {
        /// Every row of the role-to-assignment table.
        roles: Vec<RoleEntry>,
        /// Mirrors [`ClientMessage::SaveRoles::default_role`]: the table as
        /// stored.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_role: Option<String>,
    },
    /// Response to [`ClientMessage::OpenTaskWindow`].
    TaskWindowOpened {
        /// The plan holding the task.
        plan_id: String,
        /// The task the window was opened for.
        task_id: String,
        /// The newly opened window.
        window_id: String,
        /// The assignment the window was launched with, and where it came
        /// from.
        assignment: ResolvedAssignment,
    },
    /// Sent to every registered approver when a plan's stored content
    /// changes, so a displayed copy can be read again.
    PlanChanged {
        /// The plan that changed.
        plan_id: String,
    },
    /// Response to [`ClientMessage::SendReport`].
    ReportSent {
        /// The window the report was delivered to. Always present on the
        /// wire, as `null` when no window received it.
        delivered_to: Option<String>,
        /// The plan task the report was recorded against. Always present on
        /// the wire, as `null` when the reporting window runs no plan task.
        task: Option<ReportTaskRef>,
    },
    /// Response to [`ClientMessage::WaitForReport`]: every report taken, in
    /// arrival order; empty when none arrived in time.
    Reports {
        /// The reports taken from the calling window's inbox.
        reports: Vec<ReceivedReport>,
        /// The windows that would report to the caller and have not exited,
        /// so a caller that received nothing can tell who it is waiting on.
        /// `None` when the daemon does not report this, which is not the
        /// same as waiting on no window. Omitted from the wire when `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        awaiting: Option<Vec<AwaitingWindow>>,
    },
    /// Immediate answer to [`ClientMessage::ListModelChoices`]: the request
    /// was accepted, and [`DaemonMessage::ModelChoices`] follows.
    ModelChoicesRequested {
        /// The request's [`ClientMessage::ListModelChoices::request_id`].
        request_id: u64,
        /// The profile whose models are being listed.
        profile: String,
    },
    /// Sent to the requesting connection when the list asked for by
    /// [`ClientMessage::ListModelChoices`] is ready or could not be taken.
    ModelChoices {
        /// The request's [`ClientMessage::ListModelChoices::request_id`].
        request_id: u64,
        /// The profile whose models were listed.
        profile: String,
        /// Where the list came from. `None` when the profile declares no
        /// source; omitted from the wire (rather than sent as `null`) then.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<ModelChoiceSource>,
        /// The models found, in the source's order; empty when none were.
        choices: Vec<ModelChoice>,
        /// A one-line account of why the list could not be taken. Omitted
        /// from the wire (rather than sent as `null`) on success.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Response to [`ClientMessage::SaveModelAliases`]: the aliases as
    /// stored.
    ModelAliasesSaved {
        /// The profile whose aliases were replaced.
        profile: String,
        /// The aliases as stored.
        aliases: Vec<LaunchOptionAlias>,
    },
    /// Response to [`ClientMessage::SetProfileEnabled`]: the setting as
    /// stored.
    ProfileEnabledSet {
        /// The profile whose setting was stored.
        profile: String,
        /// The stored setting.
        enabled: bool,
    },
    /// Response to [`ClientMessage::GetProfileDetail`].
    ProfileDetail {
        /// The profile read.
        profile: String,
        /// Whether the profile is laid over a shipped profile. When `false`
        /// every value is the user's own.
        shipped_base: bool,
        /// The argv in use on the daemon's host.
        command: ProfileCommandDetail,
        /// Every value in use, in the profile's own key order.
        entries: Vec<ProfileDetailEntry>,
    },
    /// Response to [`ClientMessage::SetProfileCommand`]: the argv in use
    /// after the change.
    ProfileCommandSet {
        /// The profile changed.
        profile: String,
        /// The argv now in use.
        command: ProfileCommandDetail,
    },
    /// Response to [`ClientMessage::GetApprovalSettings`]: the daemon's
    /// current approval settings.
    ApprovalSettings {
        /// The settings read.
        settings: ApprovalSettingsInfo,
    },
    /// Response to [`ClientMessage::SetAutoApprove`]: the settings after the
    /// change.
    AutoApproveSet {
        /// The settings as stored.
        settings: ApprovalSettingsInfo,
    },
    /// Response to [`ClientMessage::SetPlanApprovalRequired`]: the settings
    /// after the change.
    PlanApprovalRequiredSet {
        /// The settings as stored.
        settings: ApprovalSettingsInfo,
    },
    /// Response to [`ClientMessage::ListMcpRegistrations`]: one row per
    /// profile that describes its registration. Each row's state follows as a
    /// [`DaemonMessage::McpRegistrationStatus`] push.
    McpRegistrationsListed {
        /// The request this answers.
        request_id: u64,
        /// The bridge executable a registration points the CLI at. `None`
        /// when the daemon found none beside itself; no row's state is read
        /// then.
        #[serde(default)]
        bridge: Option<String>,
        /// The rows, in the profile list's order.
        rows: Vec<McpRegistrationRow>,
    },
    /// Push: one row's registration state, read on behalf of a
    /// [`ClientMessage::ListMcpRegistrations`].
    McpRegistrationStatus {
        /// The request that caused this read.
        request_id: u64,
        /// The state read.
        status: McpRegistrationStatusInfo,
    },
    /// Response to [`ClientMessage::RunMcpRegistration`]: the operation was
    /// accepted and runs; its outcome follows as a
    /// [`DaemonMessage::McpRegistrationResult`] push.
    McpRegistrationRequested {
        /// The request this answers.
        request_id: u64,
        /// The profile being changed.
        profile: String,
        /// The action accepted.
        action: McpRegistrationAction,
    },
    /// Push: the outcome of a [`ClientMessage::RunMcpRegistration`].
    McpRegistrationResult {
        /// The request that caused this operation.
        request_id: u64,
        /// The profile that was changed.
        profile: String,
        /// The action that ran.
        action: McpRegistrationAction,
        /// The steps that ran, in order; a failed step is the last one.
        steps: Vec<McpRegistrationStep>,
        /// Fixed English text naming why the operation failed. `None` when it
        /// succeeded or the peer does not carry the field.
        #[serde(default)]
        error: Option<String>,
        /// The registration state read after the operation.
        status: McpRegistrationStatusInfo,
    },
}

/// The stream a hook prints a [`DaemonMessage::Delivery`] on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// Why a daemon cannot do its job, as [`DaemonProblem::kind`] names it.
///
/// Carries no catch-all variant: a client that does not know a kind shows
/// nothing for it, and a new kind comes with a new wire revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonProblemKind {
    /// The pseudoconsole bundled with the daemon is missing or altered, so no
    /// window can be opened.
    WindowsUnavailable,
}

/// One problem listed by [`DaemonMessage::DaemonHealth`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonProblem {
    /// What kind of problem this is.
    pub kind: DaemonProblemKind,
    /// The daemon's own English description of the cause, for a log.
    pub detail: String,
}

/// What [`ClientMessage::RunMcpRegistration`] does.
///
/// Carries no catch-all variant: it names the caller's intent, which must not
/// be filled in with a default, and the daemon only echoes a value it received.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpRegistrationAction {
    /// Adds the registration.
    Register,
    /// Removes the registration and adds it again.
    Update,
    /// Removes the registration.
    Remove,
}

/// One profile's registration state, as [`McpRegistrationStatusInfo::state`]
/// reports it.
///
/// `Unknown` is a serde catch-all (R3): an older client must not lose an
/// entire status frame merely because a newer daemon reports a state it
/// predates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpRegistrationState {
    /// Registered and pointing at the expected bridge and settings.
    Registered,
    /// Registered, but at least one observed value differs from the expected
    /// one (see [`McpRegistrationStatusInfo::differs_in`]).
    Differs,
    /// The CLI is installed and has no such registration.
    NotRegistered,
    /// The CLI is not installed.
    NotInstalled,
    /// The state could not be read, or is a value this build does not
    /// recognize.
    #[serde(other)]
    Unknown,
}

/// A command as the daemon would run it, for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpRegistrationCommand {
    /// The whole argv, including any launcher in front of the CLI.
    pub argv: Vec<String>,
    /// The index in `argv` at which the CLI's own executable starts.
    pub program_at: u32,
}

/// One key a registration writes into a CLI's TOML settings file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpRegistrationTomlValue {
    /// The key within the table.
    pub key: String,
    /// The value, written in TOML notation.
    pub value: String,
}

/// The settings a registration writes into a CLI's TOML settings file after
/// the CLI's own registration command, for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpRegistrationTomlSet {
    /// The file, relative to the home directory.
    pub file: String,
    /// The path of the table the values are written into.
    pub table: Vec<String>,
    /// The values written.
    pub values: Vec<McpRegistrationTomlValue>,
}

/// One row of [`DaemonMessage::McpRegistrationsListed`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpRegistrationRow {
    /// The profile identifier.
    pub profile: String,
    /// The profile's display name.
    pub display_name: String,
    /// The group the profile's registration belongs to. `None` when the
    /// profile names none or the peer does not carry the field.
    #[serde(default)]
    pub group: Option<String>,
    /// The command that adds the registration. Where the bridge path is not
    /// yet known, the element carrying it is still a placeholder.
    pub add: McpRegistrationCommand,
    /// The command that removes the registration.
    pub remove: McpRegistrationCommand,
    /// The settings written after `add`. `None` when the profile writes none
    /// or the peer does not carry the field.
    #[serde(default)]
    pub toml_set: Option<McpRegistrationTomlSet>,
    /// The hook entries registration adds to the CLI's settings files, for
    /// display. Empty when the profile writes none or the peer does not
    /// carry the field.
    #[serde(default)]
    pub hook_entries: Vec<McpRegistrationHookEntry>,
}

/// One entry a registration adds to a CLI's settings file so the CLI runs
/// yagura's hooks, for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpRegistrationHookEntry {
    /// The file, relative to the home directory.
    pub file: String,
    /// The entry's name within the file.
    pub name: String,
    /// The entry's value, in the file's own notation.
    pub content: String,
}

/// One profile's registration state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpRegistrationStatusInfo {
    /// The profile identifier.
    pub profile: String,
    /// The state read.
    pub state: McpRegistrationState,
    /// The keys whose observed value differs from the expected one; empty
    /// unless `state` is [`McpRegistrationState::Differs`].
    pub differs_in: Vec<String>,
    /// Fixed English text explaining the state. `None` when there is nothing
    /// to explain or the peer does not carry the field.
    #[serde(default)]
    pub reason: Option<String>,
    /// The resolved add command. `None` when it could not be resolved or the
    /// peer does not carry the field.
    #[serde(default)]
    pub add: Option<McpRegistrationCommand>,
    /// The resolved remove command, with the same absence semantics as
    /// `add`.
    #[serde(default)]
    pub remove: Option<McpRegistrationCommand>,
}

/// One step of a [`DaemonMessage::McpRegistrationResult`].
///
/// A step that reads or writes a file rather than running a command carries
/// an empty `argv`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpRegistrationStep {
    /// The argv the step ran; empty for a file step.
    pub argv: Vec<String>,
    /// The index in `argv` at which the CLI's own executable starts.
    pub program_at: u32,
    /// The exit code. `None` when the step ran no command, the command did
    /// not exit with a code, or the peer does not carry the field.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// The step's sanitized output, or a fixed description of a file step.
    pub output: String,
}

/// Whether the daemon's playbook (the source of its approval policy) is
/// loaded, and if not, why.
///
/// `Unknown` is a serde catch-all (R3): an older build of the caller must not
/// lose an entire [`ApprovalSettingsInfo`] frame merely because a newer
/// daemon reports a playbook state it predates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybookState {
    /// The playbook file was found and parsed; its `[approval]` / `[plans]`
    /// sections are in effect.
    Loaded,
    /// No playbook path is configured.
    NotConfigured,
    /// A playbook path is configured, but the file could not be read or
    /// parsed.
    Failed,
    /// A value this build does not recognize.
    #[serde(other)]
    Unknown,
}

/// One destructive tool's automatic-approval setting, as carried by
/// [`ApprovalSettingsInfo::tools`].
///
/// `tool` is a plain string rather than an enum: it travels from the daemon
/// (which owns the closed set of destructive tool names) to a client that may
/// predate a tool the daemon now knows about, and a plain string lets that
/// client display the setting instead of failing to decode the whole frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolAutoApprove {
    /// The tool's name (see `DestructiveTool::as_tool_name` in the daemon
    /// crate, which owns the closed set this string is drawn from).
    pub tool: String,
    /// `true` when the tool proceeds without approval.
    pub auto_approve: bool,
}

/// The daemon's approval settings, as read by [`ClientMessage::GetApprovalSettings`]
/// and echoed by every settings-changing answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalSettingsInfo {
    /// Whether the playbook backing these settings is loaded.
    pub playbook: PlaybookState,
    /// One entry per destructive tool the daemon knows about.
    pub tools: Vec<ToolAutoApprove>,
    /// `true` when a newly created plan requires review before work begins.
    pub plans_require_approval: bool,
}

/// How the work behind a task report ended.
///
/// Carries no catch-all variant: a string outside this set fails to decode
/// rather than turning into a guessed outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportOutcome {
    /// The work was completed.
    Succeeded,
    /// The work could not be completed.
    Failed,
}

/// The plan task a report was recorded against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportTaskRef {
    /// The plan holding the task.
    pub plan_id: String,
    /// The task within that plan.
    pub task_id: String,
}

/// One report as delivered to the window it was addressed to.
///
/// Every field is always present on the wire; an absent optional value is
/// `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceivedReport {
    /// The reporting window's id.
    pub from_window_id: String,
    /// The reporting window's display name.
    pub from_name: String,
    /// Whether the work succeeded.
    pub outcome: ReportOutcome,
    /// The reporting window's account.
    pub summary: String,
    /// Where a longer result was written, if the reporter named a file.
    pub path: Option<String>,
    /// The plan task the report was recorded against, if any.
    pub task: Option<ReportTaskRef>,
    /// When the report was received, in Unix epoch milliseconds.
    pub at_ms: u64,
}

/// One window a caller of [`ClientMessage::WaitForReport`] is still waiting
/// on, as listed in [`DaemonMessage::Reports::awaiting`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwaitingWindow {
    /// The window's id.
    pub window_id: String,
    /// The window's display name.
    pub name: String,
    /// The window's current status.
    pub status: WindowStatus,
    /// Whether the window's MCP bridge is identified; see
    /// [`WindowInfo::mcp_connected`]. Omitted from the wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_connected: Option<bool>,
}

/// The latest report recorded against a plan task.
///
/// Every field is always present on the wire; an absent `path` is `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReport {
    /// Whether the work succeeded.
    pub outcome: ReportOutcome,
    /// The reporting window's account.
    pub summary: String,
    /// Where a longer result was written, if the reporter named a file.
    pub path: Option<String>,
    /// When the report was recorded, in Unix epoch seconds.
    pub reported_at: u64,
}

/// One task of a plan as the planning window writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanTaskSpec {
    /// The task's identifier, unique within its plan.
    pub task_id: String,
    /// The task's display title.
    pub title: String,
    /// The role whose row in the role table supplies an assignment.
    pub role: Option<String>,
    /// The launch profile the planning window proposes. `model` and
    /// `effort` are only meaningful together with it.
    pub profile: Option<String>,
    /// The `model` launch option the planning window proposes.
    pub model: Option<String>,
    /// The `effort` launch option the planning window proposes.
    pub effort: Option<String>,
    /// The tasks that must finish before this one starts.
    pub depends_on: Vec<String>,
    /// The paths this task is expected to write.
    pub writes: Vec<String>,
    /// A request for the window opened for this task to receive, read back
    /// through `get_plan` rather than written into the window's PTY (see
    /// [`validate_task_instruction`]). `None` when the task carries none.
    /// Omitted from the wire when `None`, so a daemon predating this field
    /// still decodes (and produces) a valid `PlanTaskSpec`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
}

/// A launch profile together with the launch options passed through it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAssignment {
    /// The launch profile a task's window opens from.
    pub profile: String,
    /// The `model` launch option, mapped through the profile.
    pub model: Option<String>,
    /// The `effort` launch option, mapped through the profile.
    pub effort: Option<String>,
}

/// Where a task's resolved assignment came from.
///
/// Carries no catch-all variant: a string outside this set fails to decode
/// rather than turning into a guessed source.
///
/// # Examples
///
/// ```
/// use yagura_protocol::AssignmentSource;
///
/// let encoded = serde_json::to_string(&AssignmentSource::Unassigned)
///     .expect("a plain enum always encodes");
/// assert_eq!(encoded, "\"unassigned\"");
/// assert!(serde_json::from_str::<AssignmentSource>("\"bogus\"").is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentSource {
    /// The user's override, set from the GUI.
    User,
    /// The planning window's own proposal.
    Parent,
    /// The row of the role table named by the task's role.
    Role,
    /// The role table's `default_role` row, used when the task names no
    /// role or names one the table has no row for.
    Default,
    /// No source supplied an assignment.
    Unassigned,
}

/// A task's effective assignment and the source it was taken from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedAssignment {
    /// Which source won.
    pub source: AssignmentSource,
    /// The winning assignment. `None` exactly when `source` is
    /// [`AssignmentSource::Unassigned`].
    pub assignment: Option<TaskAssignment>,
}

/// One task of a stored plan, as reported back to a reader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanTaskInfo {
    /// The task's identifier, unique within its plan.
    pub task_id: String,
    /// The task's display title.
    pub title: String,
    /// The role the task names, if any.
    pub role: Option<String>,
    /// The tasks that must finish before this one starts.
    pub depends_on: Vec<String>,
    /// The paths this task is expected to write.
    pub writes: Vec<String>,
    /// The index of the wave the task belongs to, counted from zero.
    pub wave: u32,
    /// The planning window's proposal, if it made one.
    pub parent: Option<TaskAssignment>,
    /// The user's override, if one is set.
    pub user: Option<TaskAssignment>,
    /// The assignment a window for this task would be opened with.
    pub resolved: ResolvedAssignment,
    /// The window last opened for this task, kept as stored even after that
    /// window closes; whether it is still alive is the reader's question.
    pub window_id: Option<String>,
    /// The latest report recorded against this task. Unlike this struct's
    /// other optional fields, omitted from the wire when `None`, so a daemon
    /// that records no reports and one that has none for this task look
    /// alike: both mean "no report is known".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<TaskReport>,
    /// The task's stored request, mirroring [`PlanTaskSpec::instruction`].
    /// `None` when the task carries none, or when the daemon predates this
    /// field. Omitted from the wire when `None`, so a peer predating this
    /// field still decodes (and produces) a valid `PlanTaskInfo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    /// Where delivering the task's `instruction` to its window stands.
    /// `None` when there is nothing to deliver, or when the daemon predates
    /// this field; omitted from the wire when `None`, so both read alike as
    /// "no delivery state is known".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction_notice: Option<InstructionNotice>,
    /// Whether this task is new or changed since the plan's owning
    /// [`PlanInfo`] was last [`PlanReview::Approved`], as recorded by
    /// `replace_tasks`. `None` when the task carries no mark (unchanged, or
    /// the plan has never been reviewed), or when the daemon predates this
    /// field. Omitted from the wire when `None`, so both read alike as "no
    /// mark is known".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_mark: Option<ReviewMark>,
}

/// The delivery state of a plan task's `instruction` notice.
///
/// Both fields are always present on the wire; an absent `at` is `null`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionNotice {
    /// Where the delivery stands.
    pub state: InstructionNoticeState,
    /// When `state` was reached, in Unix epoch seconds. `None` while the
    /// notice is [`InstructionNoticeState::Pending`].
    pub at: Option<u64>,
}

/// How far delivering a plan task's `instruction` notice has come.
///
/// Carries no catch-all variant: a string outside this set fails to decode
/// rather than turning into a guessed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionNoticeState {
    /// The notice has not been written to the task's window yet.
    Pending,
    /// The notice was written to the task's window.
    Delivered,
    /// The task's window closed before the notice was written.
    WindowClosed,
}

/// One stored plan in full.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanInfo {
    /// The plan's identifier.
    pub plan_id: String,
    /// The plan's display title.
    pub title: String,
    /// The working directory the plan's task windows open in.
    pub cwd: String,
    /// The owning window, kept as stored even after that window closes.
    /// `None` when the plan has no owner.
    pub owner_window: Option<String>,
    /// The task identifiers grouped by wave, in wave order.
    pub waves: Vec<Vec<String>>,
    /// Every task of the plan.
    pub tasks: Vec<PlanTaskInfo>,
    /// English reason text when the role table could not be read, so role
    /// rows took no part in resolving assignments.
    pub roles_warning: Option<String>,
    /// Where the plan stands in the review gate. `None` when the daemon's
    /// `plan_approval_required` policy was off when the plan was created, or
    /// when the daemon predates this field. Omitted from the wire when
    /// `None`, so both read alike as "no review is in force".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<PlanReview>,
    /// A counter `replace_tasks` increases by one on every call, starting
    /// at `0` for a plan that has never had its tasks replaced. Always
    /// present on the wire so a caller of `approve_plan` / `mark_plan_seen`
    /// can read the version to send back without a prior migration step; a
    /// daemon predating this field reports `0`.
    #[serde(default)]
    pub version: u64,
}

/// One plan in a [`DaemonMessage::PlanList`] listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSummary {
    /// The plan's identifier.
    pub plan_id: String,
    /// The plan's display title.
    pub title: String,
    /// The owning window, kept as stored even after that window closes.
    pub owner_window: Option<String>,
    /// How many tasks the plan holds.
    pub task_count: u32,
    /// Mirrors [`PlanInfo::review`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<PlanReview>,
    /// Whether any of the plan's tasks carry a [`PlanTaskInfo::review_mark`]
    /// the caller has not yet acknowledged through `mark_plan_seen`. Always
    /// present on the wire; a daemon predating this field reports `false`.
    #[serde(default)]
    pub unseen_changes: bool,
}

/// Where a plan stands in the review gate that `plan_approval_required`
/// turns on: a newly created plan starts [`PlanReview::Pending`] and an
/// approver settles it to [`PlanReview::Approved`] with `approve_plan`.
///
/// Carries no catch-all variant: a string outside this set fails to decode
/// rather than turning into a guessed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanReview {
    /// Waiting for an approver's `approve_plan`. `open_task_window` is
    /// refused for this plan's tasks while it stands here.
    Pending,
    /// Settled by an approver's `approve_plan`.
    Approved,
}

/// Whether a plan task is new or changed since the plan was last approved,
/// as [`PlanTaskInfo::review_mark`] reports it.
///
/// Carries no catch-all variant: a string outside this set fails to decode
/// rather than turning into a guessed mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewMark {
    /// The task did not exist in the plan's previously approved tasks.
    New,
    /// The task existed before, but one of its reviewed fields differs.
    Changed,
}

/// One row of the role-to-assignment table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleEntry {
    /// The role's name, as tasks refer to it.
    pub name: String,
    /// The launch profile the role's tasks open from.
    pub profile: String,
    /// The `model` launch option, mapped through the profile.
    pub model: Option<String>,
    /// The `effort` launch option, mapped through the profile.
    pub effort: Option<String>,
}

/// One message in a chat room, as returned by
/// [`ClientMessage::ReadChat`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// This message's sequence number within its room.
    pub seq: u64,
    /// When this message was posted, in Unix epoch milliseconds.
    pub at_ms: u64,
    /// Who posted this message: `"window"`, `"user"`, `"external"` or
    /// `"system"`.
    pub from_kind: String,
    /// The posting window's id. `Some` only for `from_kind == "window"`.
    pub from_window_id: Option<String>,
    /// The poster's display name (`"user"` / `"external"` / `"system"` for
    /// the non-window kinds).
    pub from_name: String,
    /// The message body, verbatim.
    pub body: String,
    /// Resolved window ids this message mentioned, plus the literal string
    /// `"user"` when it mentioned `@user`.
    pub mentions: Vec<String>,
    /// Whether this message was posted through `broadcast_chat`.
    pub broadcast: bool,
    /// One delivery record per addressed window.
    pub deliveries: Vec<ChatDeliveryInfo>,
}

/// How one chat message landed in one addressed window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatDeliveryInfo {
    /// The addressed window.
    pub window_id: String,
    /// The delivery state: `"pending"`, `"taken"`, `"undelivered_unknown"`,
    /// `"undelivered_halted"`, `"undelivered_exhausted"` or
    /// `"undelivered_restart"`.
    pub state: String,
    /// Why the addressed window's status was `Unknown` at the moment this
    /// delivery was recorded, when that is why delivery did not happen.
    /// `None` when `state` carries no such reason, or when the value
    /// persisted for this row is not one this build recognizes (the row is
    /// kept; only the reason is dropped). Omitted from the
    /// wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_reason: Option<UnknownReason>,
}

/// One chat room in a [`DaemonMessage::ChatRooms`] listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRoomInfo {
    /// The room's identifier.
    pub room_id: String,
    /// The room's display name: the workspace's own name, extended with as
    /// much of its parent path as it takes to tell two same-named rooms
    /// apart. Never the whole path.
    pub name: String,
    /// How many of the daemon's current participants belong to this room.
    pub participant_count: u32,
    /// Whether the room currently needs a human's attention.
    pub needs_user: bool,
    /// The room's current halt, if any. Always present on the wire (as
    /// `null` when the room is not halted), the same discipline
    /// [`DaemonMessage::ChatMessages::halted`] follows.
    pub halted: Option<ChatHalt>,
    /// When this room was last posted to, in Unix epoch milliseconds.
    /// `None` -- present on the wire as `null` -- when nothing has been
    /// posted in it yet.
    pub last_at_ms: Option<u64>,
}

/// A room's current halt, as reported by [`DaemonMessage::ChatMessages`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatHalt {
    /// Why the room halted: `"repetition"`, `"monologue"` or `"turn_budget"`.
    pub reason: String,
    /// When the halt began, in Unix epoch milliseconds.
    pub since_ms: u64,
}

/// One chat room participant, as reported by [`DaemonMessage::ChatMessages`].
/// Never includes the human user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatParticipant {
    /// The participant window's id.
    pub window_id: String,
    /// The participant window's display name.
    pub name: String,
}

/// Upper bound (bytes) on a chat message body.
pub const CHAT_BODY_MAX_BYTES: usize = 768;

/// Upper bound accepted for [`ClientMessage::ReadChat::limit`].
pub const CHAT_READ_LIMIT_MAX: u32 = 200;

/// Why a chat message body was rejected.
///
/// A distinct type from [`HandoffTextError`] / [`SendTextError`] because a
/// chat body has its own byte budget ([`CHAT_BODY_MAX_BYTES`]) and, unlike
/// [`SendTextError`], an empty body is not a valid (no-op) post.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChatBodyError {
    /// The body has no visible character: it is empty, or every character
    /// in it is whitespace or a zero-width joiner / non-joiner.
    #[error("chat message body has no visible text; to send no reply, do not post at all")]
    Empty,
    /// The body exceeds [`CHAT_BODY_MAX_BYTES`].
    #[error("chat message body is {actual} bytes, exceeding the {max}-byte limit")]
    TooLong {
        /// The limit that was exceeded ([`CHAT_BODY_MAX_BYTES`]).
        max: usize,
        /// The body's actual length in UTF-8 bytes.
        actual: usize,
    },
    /// The body contains a character forbidden in a chat message: the same
    /// set [`is_forbidden_send_text_char`] rejects for a `send_message` body
    /// (control characters other than `\n` / `\r`), for the same reason: a
    /// chat message is written into a receiving window's PTY and read by a
    /// human approver, so a redraw-capable control character must never
    /// reach either.
    ///
    /// The character itself is reported as a code point and never echoed
    /// verbatim in the message, for the same reason as
    /// [`HandoffTextError::ForbiddenChar`].
    #[error("chat message body contains the forbidden character U+{:04X} at byte offset {at}", *ch as u32)]
    ForbiddenChar {
        /// Byte offset of the offending character within the body.
        at: usize,
        /// The offending character.
        ch: char,
    },
}

/// Validates a chat message body: at least one visible character, at most [`CHAT_BODY_MAX_BYTES`]
/// UTF-8 bytes, and free of the characters [`ChatBodyError::ForbiddenChar`]
/// documents. Shares its forbidden-character rule with
/// [`validate_send_text_body`] ([`is_forbidden_send_text_char`]) rather than
/// restating it.
///
/// # Errors
///
/// Returns the first violation found, checked in the order empty -> too
/// long -> forbidden character.
///
/// # Examples
///
/// ```
/// use yagura_protocol::{CHAT_BODY_MAX_BYTES, ChatBodyError, validate_chat_body};
///
/// assert_eq!(validate_chat_body("hello @user"), Ok(()));
/// assert_eq!(validate_chat_body(""), Err(ChatBodyError::Empty));
/// assert!(matches!(
///     validate_chat_body(&"a".repeat(CHAT_BODY_MAX_BYTES + 1)),
///     Err(ChatBodyError::TooLong { .. })
/// ));
/// ```
pub fn validate_chat_body(value: &str) -> Result<(), ChatBodyError> {
    if value
        .chars()
        .all(|ch| ch.is_whitespace() || matches!(ch, '\u{200C}' | '\u{200D}'))
    {
        return Err(ChatBodyError::Empty);
    }
    if value.len() > CHAT_BODY_MAX_BYTES {
        return Err(ChatBodyError::TooLong {
            max: CHAT_BODY_MAX_BYTES,
            actual: value.len(),
        });
    }
    match value
        .char_indices()
        .find(|&(_, ch)| is_forbidden_send_text_char(ch))
    {
        Some((at, ch)) => Err(ChatBodyError::ForbiddenChar { at, ch }),
        None => Ok(()),
    }
}

/// Errors returned while decoding the length-prefixed frame envelope itself
/// (independent of the JSON message contents).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// The declared body length exceeds [`MAX_FRAME_BODY_LEN`].
    #[error("frame body exceeds maximum size")]
    TooLarge,
}

/// Errors returned while decoding a JSON message body into a
/// [`ClientMessage`] or [`DaemonMessage`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MessageError {
    /// The body is not valid JSON, or a required field for the (known)
    /// message type is missing / has the wrong shape.
    #[error("invalid json or missing required field")]
    InvalidJson,
    /// The `type` field does not match any variant known to the target enum.
    /// This also covers direction-violating types (e.g. a client sending a
    /// daemon-only type), since each direction has its own enum.
    #[error("unknown message type")]
    UnknownType,
}

/// Prefixes `body` with its length as a little-endian `u32` and returns the
/// resulting frame bytes.
pub fn frame_encode(body: &[u8]) -> Vec<u8> {
    let mut framed = Vec::with_capacity(4 + body.len());
    framed.extend_from_slice(&(body.len() as u32).to_le_bytes());
    framed.extend_from_slice(body);
    framed
}

/// Attempts to extract a single length-prefixed frame from the front of
/// `buf`.
///
/// - `Ok(Some((body, consumed)))`: a complete frame was available; `body` is
///   a slice into `buf` holding the JSON payload and `consumed` is the total
///   number of bytes (prefix + body) making up that frame.
/// - `Ok(None)`: not enough data has arrived yet (either the 4-byte length
///   prefix itself, or the declared body). A declared length exactly equal
///   to [`MAX_FRAME_BODY_LEN`] falls into this case while the body is still
///   arriving.
/// - `Err(FrameError::TooLarge)`: the declared length exceeds
///   [`MAX_FRAME_BODY_LEN`]. This is determined from the 4-byte prefix alone,
///   without waiting for the body to arrive.
pub fn frame_decode(buf: &[u8]) -> Result<Option<(&[u8], usize)>, FrameError> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let mut len_bytes = [0u8; 4];
    len_bytes.copy_from_slice(&buf[..4]);
    let declared_len = u32::from_le_bytes(len_bytes) as usize;
    if declared_len > MAX_FRAME_BODY_LEN {
        return Err(FrameError::TooLarge);
    }
    let total_len = 4 + declared_len;
    if buf.len() < total_len {
        return Ok(None);
    }
    Ok(Some((&buf[4..total_len], total_len)))
}

/// Serializes `msg` to its JSON body (without the length prefix).
pub fn encode_client_message(msg: &ClientMessage) -> Vec<u8> {
    // `ClientMessage` contains only strings/numbers/bools, so serialization
    // cannot fail in practice; fall back to an empty body rather than panic.
    serde_json::to_vec(msg).unwrap_or_default()
}

/// Serializes `msg` to its JSON body (without the length prefix).
pub fn encode_daemon_message(msg: &DaemonMessage) -> Vec<u8> {
    serde_json::to_vec(msg).unwrap_or_default()
}

/// The `type` values recognized by [`ClientMessage`].
const CLIENT_MESSAGE_TYPES: &[&str] = &[
    "attach_view",
    "frame_ack",
    "set_palette",
    "set_visible",
    "copy_selection",
    "clear_selection",
    "scroll_view",
    "key",
    "text",
    "paste",
    "pointer",
    "wheel",
    "focus",
    "resize",
    "ping",
    "shutdown",
    "status_event",
    "open_window",
    "list_windows",
    "get_window_status",
    "send_text",
    "send_keys",
    "interrupt_window",
    "close_window",
    "wait_for_status",
    "rename_window",
    "read_output",
    "list_detections",
    "list_restore_candidates",
    "update_window_layout",
    "list_profiles",
    "open_window_from_profile",
    "identify",
    "register_approver",
    "approval_decision",
    "send_handoff",
    "send_chat",
    "broadcast_chat",
    "read_chat",
    "list_chat_rooms",
    "get_protocol_revision",
    "get_daemon_health",
    "create_plan",
    "update_plan",
    "get_plan",
    "list_plans",
    "delete_plan",
    "list_roles",
    "save_roles",
    "set_task_assignment",
    "clear_task_assignment",
    "set_plan_owner",
    "open_task_window",
    "approve_plan",
    "mark_plan_seen",
    "send_report",
    "wait_for_report",
    "list_model_choices",
    "save_model_aliases",
    "set_profile_enabled",
    "get_profile_detail",
    "set_profile_command",
    "get_approval_settings",
    "set_auto_approve",
    "set_plan_approval_required",
    "list_mcp_registrations",
    "run_mcp_registration",
    "deliver_context",
    "deliver_take",
    "deliver_wait",
    "report_session",
];

/// The `type` values recognized by [`DaemonMessage`].
const DAEMON_MESSAGE_TYPES: &[&str] = &[
    "attached",
    "frame",
    "selection_text",
    "exit",
    "pong",
    "error",
    "status_changed",
    "window_opened",
    "window_list",
    "window_status",
    "text_sent",
    "keys_sent",
    "window_interrupted",
    "window_closed",
    "wait_result",
    "window_renamed",
    "window_output",
    "detection_list",
    "restore_candidate_list",
    "window_layout_updated",
    "profile_list",
    "identified",
    "approver_registered",
    "approval_requested",
    "approval_withdrawn",
    "approval_resolved",
    "handoff_sent",
    "chat_posted",
    "chat_messages",
    "chat_changed",
    "chat_rooms",
    "protocol_revision",
    "daemon_health",
    "plan_detail",
    "plan_list",
    "plan_deleted",
    "role_list",
    "task_window_opened",
    "plan_changed",
    "report_sent",
    "reports",
    "model_choices_requested",
    "model_choices",
    "model_aliases_saved",
    "profile_enabled_set",
    "profile_detail",
    "profile_command_set",
    "approval_settings",
    "auto_approve_set",
    "plan_approval_required_set",
    "mcp_registrations_listed",
    "mcp_registration_status",
    "mcp_registration_requested",
    "mcp_registration_result",
    "delivery",
    "session_recorded",
];

/// Every `type` value [`decode_client_message`] accepts.
///
/// This is the decoder's own table, not a copy of it, so a reader that takes
/// inventory of the wire can never fall out of step with what is decoded.
pub fn client_message_wire_tags() -> &'static [&'static str] {
    CLIENT_MESSAGE_TYPES
}

/// Every `type` value [`decode_daemon_message`] accepts.
///
/// See [`client_message_wire_tags`].
pub fn daemon_message_wire_tags() -> &'static [&'static str] {
    DAEMON_MESSAGE_TYPES
}

/// Shared decode path for both message directions: parse as generic JSON
/// first so that an unrecognized `type` can be distinguished from a known
/// type with malformed/missing fields.
fn decode_message<T>(body: &[u8], known_types: &[&str]) -> Result<T, MessageError>
where
    T: serde::de::DeserializeOwned,
{
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| MessageError::InvalidJson)?;
    let type_field = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or(MessageError::InvalidJson)?;
    if !known_types.contains(&type_field) {
        return Err(MessageError::UnknownType);
    }
    serde_json::from_value(value).map_err(|_| MessageError::InvalidJson)
}

/// Decodes a JSON message body into a [`ClientMessage`].
///
/// A `type` field is first checked against the known client-side variants:
/// if it does not match any of them (including known daemon-only types,
/// e.g. `"output"`), [`MessageError::UnknownType`] is returned. If the type
/// is known but the JSON is otherwise malformed or missing required fields,
/// [`MessageError::InvalidJson`] is returned.
pub fn decode_client_message(body: &[u8]) -> Result<ClientMessage, MessageError> {
    decode_message(body, CLIENT_MESSAGE_TYPES)
}

/// Decodes a JSON message body into a [`DaemonMessage`].
///
/// See [`decode_client_message`] for the type-checking behavior.
pub fn decode_daemon_message(body: &[u8]) -> Result<DaemonMessage, MessageError> {
    decode_message(body, DAEMON_MESSAGE_TYPES)
}
