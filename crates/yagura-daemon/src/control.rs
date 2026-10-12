//! Destructive-operation control: the approval gate, the playbook that relaxes
//! it, the daemon-wide `StatusEvent` rate bucket and the primitives that carry
//! out `interrupt_window` / `close_window` / `read_output`.
//!
//! Everything a destructive IPC handler needs to *decide* lives here, so that
//! `ipc.rs` is left with "validate, call, translate the answer" and no policy
//! of its own. The gate is fail-closed: with no playbook loaded, every
//! destructive operation is denied.

use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use serde::Deserialize;
use serde::de::Error as _;
use thiserror::Error;
use tokio::sync::Semaphore;
use yagura_protocol::{DetectedStatus, UnknownReason};

use crate::config::{DaemonConfig, InterruptMethod};
use crate::room::{ChatLimits, ChatLimitsError};
use crate::window::{InputOrigin, WindowEntry};

/// A destructive operation the approval gate covers.
///
/// The variants map one-to-one onto the MCP tool names used in a playbook's
/// `auto_approve` list, which is an external contract: `open_window`,
/// `interrupt_window`, `close_window`, `send_message`, `send_keys`,
/// `send_handoff`, `send_chat` and `broadcast_chat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestructiveTool {
    /// Starting a new process in a new window: `open_window` itself, and the
    /// windows `send_handoff` (from a profile) and `open_task_window` open.
    /// There is no window to act on yet, so only the policy half of the gate
    /// applies ([`policy_gate`]).
    OpenWindow,
    /// Injecting the interrupt byte sequence into a window's PTY.
    InterruptWindow,
    /// Terminating a window and its whole process tree.
    CloseWindow,
    /// Writing text into a window's PTY on the caller's behalf
    /// (`ClientMessage::SendText` on the wire, `send_message` as an MCP
    /// tool).
    SendMessage,
    /// Writing named control keys into a window's PTY on the caller's behalf.
    SendKeys,
    /// Writing a handoff envelope into a window's PTY on the requester's
    /// behalf, opening that window from a launch profile first when asked to
    /// (`ClientMessage::SendHandoff` on the wire, `send_handoff` as an MCP
    /// tool).
    SendHandoff,
    /// Posting a message to one chat room -- the caller's own workspace room
    /// unless another is named -- addressed to its `@` mentions
    /// (`ClientMessage::SendChat` on the wire, `send_chat` as an MCP tool).
    SendChat,
    /// Posting a message to every chat room participant but the poster
    /// (`ClientMessage::BroadcastChat` on the wire, `broadcast_chat` as an
    /// MCP tool).
    BroadcastChat,
}

impl DestructiveTool {
    /// Every tool the gate covers, so that a caller enumerating them cannot
    /// silently miss a variant added later.
    pub const ALL: [Self; 8] = [
        Self::OpenWindow,
        Self::InterruptWindow,
        Self::CloseWindow,
        Self::SendMessage,
        Self::SendKeys,
        Self::SendHandoff,
        Self::SendChat,
        Self::BroadcastChat,
    ];

    /// The tool's external name, i.e. the spelling accepted in a playbook's
    /// `auto_approve` list.
    #[must_use]
    pub fn as_tool_name(self) -> &'static str {
        match self {
            Self::OpenWindow => "open_window",
            Self::InterruptWindow => "interrupt_window",
            Self::CloseWindow => "close_window",
            Self::SendMessage => "send_message",
            Self::SendKeys => "send_keys",
            Self::SendHandoff => "send_handoff",
            Self::SendChat => "send_chat",
            Self::BroadcastChat => "broadcast_chat",
        }
    }

    /// Whether `name` is a tool name this build knows about. Used to warn
    /// about (and then ignore) unrecognised entries in `auto_approve`, and by
    /// callers deciding whether a name is worth reporting as a typo.
    #[must_use]
    pub fn is_known_tool_name(name: &str) -> bool {
        Self::ALL.iter().any(|tool| tool.as_tool_name() == name)
    }
}

/// Why a destructive operation was refused outright, without reaching the
/// approval ledger.
///
/// A tool that reaches neither of these still needs a human, which is
/// [`GateVerdict::NeedsApproval`], not a [`DenyReason`]: the finer diagnosis
/// of *why* no auto-approval was available (no playbook, tool not listed) is
/// derived by `ipc.rs` from the configuration, so the gate itself stays a
/// pure decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DenyReason {
    /// No playbook approves this tool.
    #[error("approval required")]
    ApprovalRequired,
    /// The window's status could not be detected, and the requested tool is
    /// not safe to fire blind. Carries why; the `#[error]`
    /// text does not name it, so the wire error code's own message stays
    /// unchanged (`deny_msg` builds the reason-carrying refusal text
    /// separately).
    #[error("window status is unknown")]
    StatusUnknown(UnknownReason),
    /// A playbook was configured but failed to load. Distinct from
    /// [`DenyReason::ApprovalRequired`] so the caller can tell "nobody
    /// approved this" apart from "the operator's policy file is broken".
    #[error("the configured playbook failed to load")]
    PlaybookBroken,
    /// The window's detection definition declares no composer, or declares
    /// one without a probe where the tool's text must be judged, so the tool
    /// can never be carried out there. Refused before approval so no human is
    /// asked to approve an operation that cannot succeed.
    #[error("the window declares no composer")]
    NoComposer,
}

/// Where a destructive request came from, as established by the connection's
/// identity (`identify` for a window, `register_approver` for an approver).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestOrigin {
    /// No identity was established: an MCP client or CLI caller that never
    /// identified.
    External,
    /// The connection identified as the given window.
    Window(String),
    /// The connection registered as an approver.
    Approver,
}

/// The gate's outcome once no [`DenyReason`] applies outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateVerdict {
    /// The operation may run immediately.
    Allow,
    /// The operation must be queued for a human decision.
    NeedsApproval,
}

/// The playbook state [`check_destructive`] judges against.
///
/// A borrowed enum (rather than `Option<&ApprovalPolicy>`) so that "no
/// playbook was configured" and "a playbook was configured but failed to
/// load" are two distinct inputs instead of collapsing to the same `None`.
#[derive(Debug, Clone, Copy)]
pub enum PolicySource<'a> {
    /// No `--playbook` / `YAGURA_PLAYBOOK` was given.
    NotConfigured,
    /// A playbook was configured and loaded successfully.
    Loaded(&'a ApprovalPolicy),
    /// A playbook was configured but could not be loaded.
    Failed,
}

/// A parsed playbook: the operator-authored policy that relaxes the gate.
///
/// The fields are public so that in-process tests and the daemon's own
/// startup path can build a policy without going through TOML. Unknown keys
/// are rejected at every level, so a typo is a loud load failure rather than
/// a silently ignored setting.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalPolicy {
    /// The `[approval]` table. `None` (an omitted table) approves nothing.
    pub approval: Option<ApprovalSection>,
    /// The `[limits]` table. `None` keeps every built-in default.
    pub limits: Option<PolicyLimits>,
    /// The `[chat]` table. `None` keeps every built-in default
    /// ([`room::ChatLimits::default`]).
    pub chat: Option<ChatSection>,
    /// The `[plans]` table. `None` keeps plan approval required.
    pub plans: Option<PlansSection>,
}

/// The `[approval]` table of a playbook.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalSection {
    /// Tool names that may run without a human in the loop. Names this build
    /// does not know are warned about and ignored (value-level forward
    /// compatibility); there is no wildcard.
    #[serde(default)]
    pub auto_approve: Vec<String>,
}

/// The `[limits]` table of a playbook: the operational parameters an operator
/// may override.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyLimits {
    /// How `interrupt_window` delivers the interrupt (`"bytes"` / `"signal"`).
    /// An unknown value fails the whole load rather than falling back to the
    /// default, so a mistyped method can never be silently ignored.
    pub interrupt_method: Option<InterruptMethod>,
    /// Byte sequence written to the PTY by `interrupt_window`. The escape
    /// hatch for a CLI that does not react to the default `0x03`.
    pub interrupt_bytes: Option<Vec<u8>>,
    /// Upper bound accepted for a `wait_for_status` timeout, in seconds.
    pub wait_timeout_max_secs: Option<u64>,
    /// Deadline for a pending approval, in seconds. Validated while
    /// deserializing (1..=600); a value outside that range fails the whole
    /// load rather than being clamped, so a broken deadline surfaces as
    /// [`PolicyError::Parse`] (and, through the gate, [`DenyReason::PlaybookBroken`])
    /// instead of silently taking effect.
    #[serde(default, deserialize_with = "deserialize_approval_timeout_secs")]
    pub approval_timeout_secs: Option<u64>,
    /// Deadline for a handoff's target window to become ready for input, in
    /// seconds. Validated the same way, and against the same range, as
    /// [`PolicyLimits::approval_timeout_secs`].
    #[serde(default, deserialize_with = "deserialize_handoff_ready_timeout_secs")]
    pub handoff_ready_timeout_secs: Option<u64>,
    /// Upper bound on how many chat deliveries (posts and intros together)
    /// may be in flight at once (1..=256). Validated the same
    /// way as the other `[limits]` deadlines.
    #[serde(default, deserialize_with = "deserialize_chat_delivery_max")]
    pub chat_delivery_max: Option<u64>,
}

/// The `[chat]` table of a playbook: the seven numeric knobs the bundled
/// `playbook.toml` documents, plus `intro` and `exclude_detections`. Carried as
/// raw `Option<u64>` values with no range check of its own -- the single
/// range check lives in [`room::ChatLimits::validate`], reached through
/// [`chat_limits_from_section`], so the range is never repeated here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatSection {
    /// See `[chat] repetition_count` in the bundled `playbook.toml`.
    pub repetition_count: Option<u64>,
    /// See `[chat] repetition_window`.
    pub repetition_window: Option<u64>,
    /// See `[chat] repetition_distance`.
    pub repetition_distance: Option<u64>,
    /// See `[chat] monologue_max`.
    pub monologue_max: Option<u64>,
    /// See `[chat] turn_message_max`.
    pub turn_message_max: Option<u64>,
    /// See `[chat] silence_timeout_secs`.
    pub silence_timeout_secs: Option<u64>,
    /// See `[chat] history_max`.
    pub history_max: Option<u64>,
    /// See `[chat] intro`. `None` keeps the built-in default text; `Some("")`
    /// disables intro delivery.
    pub intro: Option<String>,
    /// See `[chat] exclude_detections`.
    pub exclude_detections: Option<Vec<String>>,
}

/// The `[plans]` table of a playbook.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlansSection {
    /// Whether a new plan waits for the operator's approval before any of its
    /// task windows can open. `None` keeps approval required.
    pub require_approval: Option<bool>,
}

/// Whether a new plan waits for approval when no playbook says otherwise.
pub const PLAN_APPROVAL_REQUIRED_BY_DEFAULT: bool = true;

impl PlansSection {
    /// Whether `section` requires plan approval: an absent table or key
    /// keeps [`PLAN_APPROVAL_REQUIRED_BY_DEFAULT`].
    #[must_use]
    pub fn requires_approval(section: Option<&Self>) -> bool {
        section
            .and_then(|section| section.require_approval)
            .unwrap_or(PLAN_APPROVAL_REQUIRED_BY_DEFAULT)
    }
}

/// Lower and upper bound (inclusive) accepted for every `[limits]` deadline
/// expressed in seconds.
const TIMEOUT_SECS_RANGE: std::ops::RangeInclusive<u64> = 1..=600;

/// Checks one `[limits]` deadline against [`TIMEOUT_SECS_RANGE`], naming
/// `key` in the rejection so an operator is told which setting was refused.
///
/// # Errors
///
/// Returns the message to report when `value` falls outside the range.
fn check_timeout_range(value: u64, key: &'static str) -> Result<(), String> {
    if TIMEOUT_SECS_RANGE.contains(&value) {
        return Ok(());
    }
    Err(format!(
        "{key} must be between {} and {}, got {value}",
        TIMEOUT_SECS_RANGE.start(),
        TIMEOUT_SECS_RANGE.end()
    ))
}

/// Deserializes one `[limits]` deadline, rejecting a value outside
/// [`TIMEOUT_SECS_RANGE`] at load time rather than clamping it silently.
/// Only called when the key is present: `#[serde(default)]` on each field
/// already covers the omitted case.
fn deserialize_timeout_secs<'de, D>(
    deserializer: D,
    key: &'static str,
) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = u64::deserialize(deserializer)?;
    check_timeout_range(value, key).map_err(serde::de::Error::custom)?;
    Ok(Some(value))
}

fn deserialize_approval_timeout_secs<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_timeout_secs(deserializer, "approval_timeout_secs")
}

fn deserialize_handoff_ready_timeout_secs<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_timeout_secs(deserializer, "handoff_ready_timeout_secs")
}

/// Lower and upper bound (inclusive) accepted for `[limits] chat_delivery_max`.
const CHAT_DELIVERY_MAX_RANGE: std::ops::RangeInclusive<u64> = 1..=256;

/// Deserializes `[limits] chat_delivery_max`, rejecting a value outside
/// [`CHAT_DELIVERY_MAX_RANGE`] at load time rather than clamping it silently,
/// the same discipline [`deserialize_timeout_secs`] applies to the two
/// deadline fields.
fn deserialize_chat_delivery_max<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = u64::deserialize(deserializer)?;
    if !CHAT_DELIVERY_MAX_RANGE.contains(&value) {
        return Err(serde::de::Error::custom(format!(
            "chat_delivery_max must be between {} and {}, got {value}",
            CHAT_DELIVERY_MAX_RANGE.start(),
            CHAT_DELIVERY_MAX_RANGE.end()
        )));
    }
    Ok(Some(value))
}

/// Why a playbook could not be turned into an [`ApprovalPolicy`].
///
/// The two cases are kept apart so the daemon's warning says which one
/// happened; both leave the gate denying everything.
#[derive(Debug, Error)]
pub enum PolicyError {
    /// The file could not be read (including: it does not exist).
    #[error("failed to read the playbook: {0}")]
    Io(#[from] std::io::Error),
    /// The file is not a valid playbook (malformed TOML, an unknown key or a
    /// value of the wrong type).
    #[error("failed to parse the playbook: {0}")]
    Parse(#[from] toml::de::Error),
}

impl ApprovalPolicy {
    /// Whether this playbook approves `tool` without a human in the loop.
    #[must_use]
    pub fn allows(&self, tool: DestructiveTool) -> bool {
        self.approval.as_ref().is_some_and(|section| {
            section
                .auto_approve
                .iter()
                .any(|name| name == tool.as_tool_name())
        })
    }

    /// The `[limits]` table, if the playbook carries one.
    #[must_use]
    pub fn limits(&self) -> Option<&PolicyLimits> {
        self.limits.as_ref()
    }
}

/// Parses a playbook from TOML source.
///
/// Unrecognised tool names inside `auto_approve` are warned about and left in
/// place (they simply match no tool); every other deviation from the schema,
/// including an unknown key or section, is an error, so a mistyped setting
/// can never be mistaken for an approval.
///
/// # Errors
///
/// Returns [`PolicyError::Parse`] when `source` is not a valid playbook.
pub fn parse_policy(source: &str) -> Result<ApprovalPolicy, PolicyError> {
    let policy: ApprovalPolicy = toml::from_str(source)?;
    warn_about_unknown_tool_names(&policy);
    if let Err(err) = chat_limits_from_section(policy.chat.as_ref()) {
        return Err(PolicyError::Parse(toml::de::Error::custom(err.to_string())));
    }
    Ok(policy)
}

/// Turns a playbook's `[chat]` table into validated [`ChatLimits`]:
/// [`ChatLimits::default`] fills every key the section omits (or the section
/// itself is `None`), and [`ChatLimits::validate`] is the single place the
/// `[chat]` ranges are checked.
///
/// # Errors
///
/// Returns the first out-of-range field [`ChatLimits::validate`] finds.
pub fn chat_limits_from_section(
    section: Option<&ChatSection>,
) -> Result<ChatLimits, ChatLimitsError> {
    let defaults = ChatLimits::default();
    let limits = match section {
        Some(section) => ChatLimits {
            repetition_count: section
                .repetition_count
                .unwrap_or(defaults.repetition_count),
            repetition_window: section
                .repetition_window
                .unwrap_or(defaults.repetition_window),
            repetition_distance: section
                .repetition_distance
                .unwrap_or(defaults.repetition_distance),
            monologue_max: section.monologue_max.unwrap_or(defaults.monologue_max),
            turn_message_max: section
                .turn_message_max
                .unwrap_or(defaults.turn_message_max),
            silence_timeout_secs: section
                .silence_timeout_secs
                .unwrap_or(defaults.silence_timeout_secs),
            history_max: section.history_max.unwrap_or(defaults.history_max),
        },
        None => defaults,
    };
    limits.validate()?;
    Ok(limits)
}

/// Merges a playbook's `[chat]` table into `config`: the
/// numeric limits go through [`chat_limits_from_section`] (already validated
/// by `parse_policy`, so this never fails), and `intro` / `exclude_detections`
/// are written when specified, otherwise left at the built-in default.
pub fn apply_chat_section(config: &mut DaemonConfig, section: Option<&ChatSection>) {
    // `parse_policy` already ran this and rejected an invalid section before
    // a `DaemonConfig` carrying it could exist, so a fresh out-of-range value
    // reaching here is a logic error rather than an operator mistake; falling
    // back to the built-in defaults is the fail-safe answer either way.
    config.chat_limits = chat_limits_from_section(section).unwrap_or_default();
    if let Some(section) = section {
        if let Some(intro) = section.intro.as_ref() {
            config.chat_intro = intro.clone();
        }
        if let Some(exclude) = section.exclude_detections.as_ref() {
            config.chat_exclude_detections = exclude.clone();
        }
    }
}

/// Merges a playbook's `[plans]` table into `config`. Only an explicit
/// `require_approval` changes the setting: an absent key or table leaves the
/// built-in default (approval required) in place.
pub fn apply_plans_section(config: &mut DaemonConfig, section: Option<&PlansSection>) {
    if let Some(required) = section.and_then(|section| section.require_approval) {
        config.plan_approval_required = required;
    }
}

/// Reads and parses the playbook at `path`.
///
/// # Errors
///
/// Returns [`PolicyError::Io`] when the file cannot be read (a non-existent
/// path lands here) and [`PolicyError::Parse`] when its contents are not a
/// valid playbook.
pub fn load_policy(path: &Path) -> Result<ApprovalPolicy, PolicyError> {
    let source = std::fs::read_to_string(path)?;
    parse_policy(&source)
}

fn warn_about_unknown_tool_names(policy: &ApprovalPolicy) {
    let Some(section) = policy.approval.as_ref() else {
        return;
    };
    for name in &section.auto_approve {
        if !DestructiveTool::is_known_tool_name(name) {
            tracing::warn!(
                "playbook auto_approve lists an unknown tool name {name:?}; ignoring it"
            );
        }
    }
}

/// Decides whether `tool`, injecting input via `status`, is safe to run
/// blind.
///
/// Covers [`DestructiveTool::InterruptWindow`], [`DestructiveTool::SendMessage`],
/// [`DestructiveTool::SendKeys`], [`DestructiveTool::SendHandoff`],
/// [`DestructiveTool::SendChat`] and [`DestructiveTool::BroadcastChat`]: each
/// of these writes into a window's PTY or posts a chat message, so
/// firing them at a window whose state could not be detected risks hitting an
/// unexpected prompt. [`DestructiveTool::CloseWindow`] is deliberately
/// exempt, so a hung or undetectable window can still be cleaned up, and
/// [`DestructiveTool::OpenWindow`] has no window whose status could be read.
/// `Exited` is not this gate's business either: the handler's
/// pre-validation refuses it before reaching here.
///
/// # Errors
///
/// Returns [`DenyReason::StatusUnknown`], carrying why, when `tool` writes
/// input and `status` is `Unknown`.
pub fn status_gate(tool: DestructiveTool, status: DetectedStatus) -> Result<(), DenyReason> {
    let Some(reason) = status.unknown_reason() else {
        return Ok(());
    };
    if matches!(
        tool,
        DestructiveTool::InterruptWindow
            | DestructiveTool::SendMessage
            | DestructiveTool::SendKeys
            | DestructiveTool::SendHandoff
            | DestructiveTool::SendChat
            | DestructiveTool::BroadcastChat
    ) {
        return Err(DenyReason::StatusUnknown(reason));
    }
    Ok(())
}

/// What the daemon may type into a window, as its detection definition
/// declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerAccess {
    /// The definition declares no `[composer]`: the daemon types nothing of
    /// its own composition.
    Absent,
    /// A `[composer]` without an `[input].probe`: the daemon may write a body
    /// it has no line to judge, but cannot tell whether a line showed.
    RawOnly,
    /// A `[composer]` and a probe character: the daemon may write and judge a
    /// line.
    Judged {
        /// The character written behind a body to learn whether it showed.
        probe: char,
    },
}

impl ComposerAccess {
    /// Whether the daemon may write a line into the window and judge whether
    /// it showed: the access every judged delivery needs.
    #[must_use]
    pub fn is_judged(self) -> bool {
        matches!(self, Self::Judged { .. })
    }
}

/// The shape of the text a tool asks the daemon to type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerWrite {
    /// One line the daemon judges against the composer before pressing the
    /// submit key.
    Judged,
    /// Bytes written as they are, with nothing to judge.
    Raw,
}

/// Decides whether `tool`, writing text shaped as `write`, can be carried out
/// at all in a window with the given composer `access`.
///
/// Only [`DestructiveTool::SendMessage`] and [`DestructiveTool::SendHandoff`]
/// type daemon-composed text into the window. A handoff and a one-line
/// message are judged against the composer, which needs a probe
/// ([`ComposerAccess::Judged`]); a message with no line to judge needs only a
/// composer. [`DestructiveTool::SendKeys`] writes the caller's bytes
/// verbatim, and the chat tools post to a room rather than to a window, so
/// they are not this gate's business.
///
/// # Errors
///
/// Returns [`DenyReason::NoComposer`] when `tool` types text that `access`
/// does not allow.
///
/// # Examples
///
/// ```
/// use yagura_daemon::control::{
///     ComposerAccess, ComposerWrite, DenyReason, DestructiveTool, composer_gate,
/// };
///
/// assert_eq!(
///     composer_gate(DestructiveTool::SendMessage, ComposerAccess::Absent, ComposerWrite::Raw),
///     Err(DenyReason::NoComposer)
/// );
/// assert_eq!(
///     composer_gate(DestructiveTool::SendMessage, ComposerAccess::RawOnly, ComposerWrite::Raw),
///     Ok(())
/// );
/// assert_eq!(
///     composer_gate(DestructiveTool::SendMessage, ComposerAccess::RawOnly, ComposerWrite::Judged),
///     Err(DenyReason::NoComposer)
/// );
/// assert_eq!(
///     composer_gate(DestructiveTool::SendKeys, ComposerAccess::Absent, ComposerWrite::Raw),
///     Ok(())
/// );
/// ```
pub fn composer_gate(
    tool: DestructiveTool,
    access: ComposerAccess,
    write: ComposerWrite,
) -> Result<(), DenyReason> {
    let allowed = match tool {
        DestructiveTool::SendHandoff => matches!(access, ComposerAccess::Judged { .. }),
        DestructiveTool::SendMessage => match write {
            ComposerWrite::Judged => matches!(access, ComposerAccess::Judged { .. }),
            ComposerWrite::Raw => !matches!(access, ComposerAccess::Absent),
        },
        DestructiveTool::OpenWindow
        | DestructiveTool::InterruptWindow
        | DestructiveTool::CloseWindow
        | DestructiveTool::SendKeys
        | DestructiveTool::SendChat
        | DestructiveTool::BroadcastChat => true,
    };
    if allowed {
        Ok(())
    } else {
        Err(DenyReason::NoComposer)
    }
}

/// Decides whether `tool`, writing text shaped as `write`, may run against
/// `target_window_id`, currently in `status` and with the composer `access`
/// its definition gives, on behalf of `origin`.
///
/// Judgement order: 1. [`status_gate`] (origin-independent) -> 2.
/// [`composer_gate`] (origin-independent) -> 3. `origin` is the target window
/// itself -> `Allow` -> 4. `origin` is a registered approver -> `Allow` -> 5.
/// `policy` approves `tool` -> `Allow`; `policy` is [`PolicySource::Failed`]
/// -> [`DenyReason::PlaybookBroken`] -> 6. [`GateVerdict::NeedsApproval`].
///
/// # Errors
///
/// Returns [`DenyReason::StatusUnknown`] when [`status_gate`] refuses,
/// [`DenyReason::NoComposer`] when [`composer_gate`] refuses, and
/// [`DenyReason::PlaybookBroken`] when no identity relieves the request and
/// the configured playbook failed to load.
pub fn check_destructive(
    policy: PolicySource<'_>,
    tool: DestructiveTool,
    status: DetectedStatus,
    access: ComposerAccess,
    write: ComposerWrite,
    origin: &RequestOrigin,
    target_window_id: &str,
) -> Result<GateVerdict, DenyReason> {
    status_gate(tool, status)?;
    composer_gate(tool, access, write)?;
    policy_gate(policy, tool, origin, Some(target_window_id))
}

/// The policy half of [`check_destructive`]: who is asking and what the
/// playbook says, with the status gate already passed (or, for a window that
/// does not exist yet, not applicable).
///
/// `target_window_id` is `None` when the target is about to be opened, in
/// which case the requester cannot be addressing its own window.
/// [`check_destructive`] is exactly [`status_gate`] and [`composer_gate`]
/// followed by this function, so a caller that has no window yet can ask the
/// same question and get the same answer.
///
/// # Errors
///
/// Returns the [`DenyReason`] the caller must report to the client.
///
/// # Examples
///
/// ```
/// use yagura_daemon::control::{
///     DestructiveTool, GateVerdict, PolicySource, RequestOrigin, policy_gate,
/// };
///
/// // No playbook and no approver identity: the operation needs a human.
/// assert_eq!(
///     policy_gate(
///         PolicySource::NotConfigured,
///         DestructiveTool::SendHandoff,
///         &RequestOrigin::External,
///         None,
///     ),
///     Ok(GateVerdict::NeedsApproval)
/// );
/// // An approver never needs approving.
/// assert_eq!(
///     policy_gate(
///         PolicySource::NotConfigured,
///         DestructiveTool::SendHandoff,
///         &RequestOrigin::Approver,
///         None,
///     ),
///     Ok(GateVerdict::Allow)
/// );
/// ```
pub fn policy_gate(
    policy: PolicySource<'_>,
    tool: DestructiveTool,
    origin: &RequestOrigin,
    target_window_id: Option<&str>,
) -> Result<GateVerdict, DenyReason> {
    if let RequestOrigin::Window(window_id) = origin
        && Some(window_id.as_str()) == target_window_id
    {
        return Ok(GateVerdict::Allow);
    }
    if matches!(origin, RequestOrigin::Approver) {
        return Ok(GateVerdict::Allow);
    }
    match policy {
        PolicySource::Loaded(policy) if policy.allows(tool) => Ok(GateVerdict::Allow),
        PolicySource::Failed => Err(DenyReason::PlaybookBroken),
        PolicySource::NotConfigured | PolicySource::Loaded(_) => Ok(GateVerdict::NeedsApproval),
    }
}

/// Merges a playbook's `[limits]` table into `config`.
///
/// Only the fields the playbook actually specifies are overridden; anything
/// omitted keeps the built-in default. The daemon calls this once at startup,
/// so the rest of the code reads plain configuration values and never has to
/// know a playbook existed.
pub fn apply_limits(config: &mut DaemonConfig, limits: Option<&PolicyLimits>) {
    let Some(limits) = limits else {
        return;
    };
    if let Some(method) = limits.interrupt_method {
        config.interrupt_method = method;
    }
    if let Some(bytes) = limits.interrupt_bytes.as_ref() {
        config.interrupt_bytes = bytes.clone();
    }
    if let Some(seconds) = limits.wait_timeout_max_secs {
        config.wait_timeout_max_secs = seconds;
    }
    if let Some(seconds) = limits.approval_timeout_secs {
        config.approval_timeout_secs = seconds;
    }
    if let Some(seconds) = limits.handoff_ready_timeout_secs {
        config.handoff_ready_timeout_secs = seconds;
    }
    if let Some(max) = limits.chat_delivery_max {
        config.chat_delivery_max = usize::try_from(max).unwrap_or(usize::MAX);
    }
}

/// Token bucket limiting how many `StatusEvent` messages the daemon accepts.
///
/// One bucket is shared daemon-wide, so reconnecting does not reset the
/// allowance. The current time is passed in rather than read from the clock,
/// which keeps the refill behaviour testable without sleeping.
#[derive(Debug)]
pub struct RateBucket {
    capacity: f64,
    refill_per_sec: f64,
    tokens: f64,
    last_refill: Option<Instant>,
}

impl RateBucket {
    /// Creates a bucket that starts full at `burst` tokens and regains
    /// `per_sec` tokens per second. A `burst` of zero admits nothing.
    #[must_use]
    pub fn new(burst: u32, per_sec: u32) -> Self {
        Self {
            capacity: f64::from(burst),
            refill_per_sec: f64::from(per_sec),
            tokens: f64::from(burst),
            last_refill: None,
        }
    }

    /// Takes one token if the bucket holds one at `now`, refilling first for
    /// the time elapsed since the previous call.
    ///
    /// Returns whether the caller may proceed. A rejection is a throttle, not
    /// a penalty: the next call after enough time has passed succeeds again.
    pub fn try_acquire(&mut self, now: Instant) -> bool {
        self.refill(now);
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }

    fn refill(&mut self, now: Instant) {
        let previous = self.last_refill.replace(now);
        let Some(previous) = previous else {
            return;
        };
        let elapsed = now.saturating_duration_since(previous).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
    }
}

/// Why a control primitive could not complete.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ControlError {
    /// The window's child process has already exited.
    #[error("child process has exited")]
    ChildExited,
    /// Writing to the PTY failed. The payload is a short, non-sensitive
    /// summary meant for a log line, never for the client.
    #[error("failed to write to the pty: {0}")]
    WriteFailed(String),
    /// The configured interrupt method is not implemented by this build.
    /// Refused rather than served by the other method, so a selection that
    /// cannot be honoured is visible instead of silently substituted.
    #[error("the configured interrupt method is not supported")]
    UnsupportedInterruptMethod,
}

/// Interrupts `entry`'s child the way `config` prescribes.
///
/// Both the method (`DaemonConfig::interrupt_method`) and the byte sequence it
/// uses (`DaemonConfig::interrupt_bytes`) are configuration, overridable
/// through a playbook's `[limits]`, rather than constants: whether `0x03`
/// interrupts a given CLI depends on that CLI's console input mode, and which
/// delivery mechanism reaches it depends on the platform.
///
/// # Errors
///
/// Returns [`ControlError::ChildExited`] when the child is already gone,
/// [`ControlError::WriteFailed`] when the PTY write fails and
/// [`ControlError::UnsupportedInterruptMethod`] when the configured method has
/// no implementation ([`InterruptMethod::Signal`]).
pub async fn interrupt_window(
    entry: &WindowEntry,
    config: &DaemonConfig,
) -> Result<(), ControlError> {
    match config.interrupt_method {
        InterruptMethod::Bytes => entry
            .write_pty_input(&config.interrupt_bytes, InputOrigin::Control)
            .await
            .map_err(|err| match err {
                crate::pty::PtyError::ChildExited => ControlError::ChildExited,
                other => ControlError::WriteFailed(other.to_string()),
            }),
        InterruptMethod::Signal => Err(ControlError::UnsupportedInterruptMethod),
    }
}

/// Closes `entry`, guaranteeing that no descendant of its child survives.
///
/// The two orderings differ. Graceful (`force = false`) asks the session to
/// terminate, spends up to `grace` waiting for the child's exit
/// notification, and only then ends the process tree, so a child that is
/// still flushing is not killed mid-flush. Forced (`force = true`) ends the
/// process tree first and terminates the session afterwards, which
/// releases the PTY reader/writer and marks the window exited.
///
/// The tree is ended on both paths and without a liveness check: ending it is
/// idempotent, so this path may race the session's own exit monitor, which
/// ends the same tree when the child exits by itself. Whichever runs first,
/// closing an already-exited window still succeeds and leaves no descendant
/// behind.
///
/// # Errors
///
/// Currently never returns an error; the result type is kept so that a future
/// failure to release the process tree can be reported without changing every
/// call site.
pub async fn close_window(
    entry: &WindowEntry,
    force: bool,
    grace: Duration,
) -> Result<(), ControlError> {
    if force {
        terminate_tree(entry);
        entry.session().terminate();
        return Ok(());
    }
    entry.session().terminate();
    let mut exited = entry.session().exit_notified();
    if !*exited.borrow_and_update() {
        let _ = tokio::time::timeout(grace, exited.changed()).await;
    }
    terminate_tree(entry);
    Ok(())
}

/// Ends `entry`'s process tree; a failure is only logged, because the
/// session's own exit monitor ends the same tree again once the child exits.
fn terminate_tree(entry: &WindowEntry) {
    if let Err(error) = entry.session().terminate_tree() {
        tracing::warn!(%error, "failed to end the window's process tree");
    }
}

/// Reads `entry`'s text as the window shows it, as its last `lines` logical
/// lines cut to `max_bytes`; the flag reports that text was left out (see
/// [`crate::detector::VtScreen::text_tail`]).
///
/// The read runs on a blocking thread, admitted by `permits`, so neither a
/// flooding window nor many readers at once hold up the async runtime.
/// Returns `None` when the read could not run (`permits` is closed or the
/// runtime is shutting down).
pub async fn read_output(
    entry: &WindowEntry,
    lines: usize,
    max_bytes: usize,
    permits: &Semaphore,
) -> Option<(String, bool)> {
    let _permit = permits.acquire().await.ok()?;
    entry.session().text_tail(lines, max_bytes).await
}
