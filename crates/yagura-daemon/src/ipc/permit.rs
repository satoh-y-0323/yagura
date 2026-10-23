//! The licence one destructive request carries from its gate to the write
//! that gate allowed.
//!
//! A [`Permit`] is built nowhere but here, by a function that judges the
//! request itself, and every path that acts on a window on a caller's behalf
//! takes one by value. "This request passed its gate" is therefore a fact the
//! compiler checks rather than one a reader has to confirm handler by
//! handler, and the tool a permit licenses is part of its type, so one
//! operation's licence cannot be spent on another's.

use std::marker::PhantomData;
use std::time::Duration;

use tokio::sync::watch;
use yagura_protocol::{
    APPROVAL_TEXT_MAX_BYTES, ApprovalTarget, ControlKey, DaemonMessage, DetectedStatus,
};

use crate::approval::{ApprovalDraft, SubmitError, Ticket};
use crate::config::DaemonConfig;
use crate::control::{self, ComposerWrite, DenyReason, DestructiveTool, GateVerdict};
use crate::live_policy;
use crate::window::WindowEntry;

use super::{
    ApprovalEnd, GateContext, HandoffSlot, PendingAction, approval_limits, await_approval,
    deny_msg, deny_no_approver, error_msg, requester_for, single_line_body, window_info,
};

/// One gated tool, named as a type so that a permit can be tied to it.
pub(super) trait GatedTool {
    /// The tool the gates judge a request for this marker against.
    const TOOL: DestructiveTool;
}

/// Writing text into a window's PTY on a caller's behalf.
pub(super) struct SendMessage;

/// Writing named control keys into a window's PTY on a caller's behalf.
pub(super) struct SendKeys;

/// Injecting the interrupt sequence into a window's PTY.
pub(super) struct InterruptWindow;

/// Terminating a window and its whole process tree.
pub(super) struct CloseWindow;

/// Handing a handoff envelope to a window's CLI.
pub(super) struct SendHandoff;

/// Starting a new process in a new window.
pub(super) struct OpenWindow;

/// Starting a new window from a profile with a handoff envelope as its first
/// request: one decision licenses both.
pub(super) struct LaunchHandoff;

impl GatedTool for SendMessage {
    const TOOL: DestructiveTool = DestructiveTool::SendMessage;
}

impl GatedTool for SendKeys {
    const TOOL: DestructiveTool = DestructiveTool::SendKeys;
}

impl GatedTool for InterruptWindow {
    const TOOL: DestructiveTool = DestructiveTool::InterruptWindow;
}

impl GatedTool for CloseWindow {
    const TOOL: DestructiveTool = DestructiveTool::CloseWindow;
}

impl GatedTool for SendHandoff {
    const TOOL: DestructiveTool = DestructiveTool::SendHandoff;
}

impl GatedTool for OpenWindow {
    const TOOL: DestructiveTool = DestructiveTool::OpenWindow;
}

impl GatedTool for LaunchHandoff {
    const TOOL: DestructiveTool = DestructiveTool::SendHandoff;
}

/// Proof that one request for the tool `T` names passed the gates that
/// license it.
///
/// Carries no data: it is the licence itself. Nothing outside this module can
/// build one, and an executor takes it by value, so a write cannot be reached
/// without a gate having run and one gate cannot license two writes.
pub(super) struct Permit<T: GatedTool> {
    tool: PhantomData<T>,
}

impl<T: GatedTool> Permit<T> {
    /// Issues the licence for `T`. Private on purpose: every caller reaches
    /// it through one of this module's gates, so no path can hold a permit
    /// the judgment never granted.
    fn issue() -> Self {
        Self { tool: PhantomData }
    }
}

impl Permit<LaunchHandoff> {
    /// The two licences the one decision granted: starting the window, and
    /// handing it the envelope.
    pub(super) fn split(self) -> (Permit<OpenWindow>, Permit<SendHandoff>) {
        (Permit::issue(), Permit::issue())
    }
}

/// Outcome of running one destructive request past the status, composer and
/// approval gates.
// Built once per request and consumed at once; boxing the answer would put an
// indirection in every caller's pattern and buy nothing.
#[allow(clippy::large_enum_variant)]
pub(super) enum GateOutcome<T: GatedTool> {
    /// The operation may run now, under the permit it carries.
    Proceed(Permit<T>),
    /// The operation is queued for a human decision; the client is answered
    /// once the request ends.
    Pending(Ticket),
    /// The operation was refused, carrying the answer the caller must send.
    /// The gate does not send it itself, so that a caller holding resources
    /// on the request's behalf can release them first.
    Refused(DaemonMessage),
}

/// Runs a resolved destructive request past the status and approval gates,
/// reporting the verdict rather than sending it.
///
/// The gate is reached from a single place per handler, which is what keeps
/// the destructive paths from drifting apart. It answers
/// nobody itself, so a caller holding a window or a budget slot on the
/// request's behalf can give those back before the refusal reaches the
/// client.
///
/// `_tool` names the tool as a type; the marker carries no data and is there
/// so the permit an allowed request receives is the one its own tool needs.
pub(super) fn gate_destructive<T: GatedTool>(
    context: &GateContext<'_>,
    entry: &WindowEntry,
    _tool: T,
    text: Option<&str>,
) -> GateOutcome<T> {
    let tool = T::TOOL;
    let detected = entry.current_detected();
    let policy_snapshot = context.policy.snapshot();
    let verdict = control::check_destructive(
        live_policy::policy_source(&policy_snapshot),
        tool,
        detected,
        entry.composer_access(),
        send_message_write(tool, text),
        context.origin,
        entry.window_id(),
    );
    gate_window(context, entry, detected, verdict, text)
}

/// Runs a handoff to an existing window past the status and approval gates.
///
/// The composer gate does not apply: the envelope goes into the window's
/// inbox, and is typed only into a window whose composer the daemon can judge.
pub(super) fn gate_handoff(
    context: &GateContext<'_>,
    entry: &WindowEntry,
    text: &str,
) -> GateOutcome<SendHandoff> {
    let tool = SendHandoff::TOOL;
    let detected = entry.current_detected();
    let policy_snapshot = context.policy.snapshot();
    let verdict = control::status_gate(tool, detected).and_then(|()| {
        control::policy_gate(
            live_policy::policy_source(&policy_snapshot),
            tool,
            context.origin,
            Some(entry.window_id()),
        )
    });
    gate_window(context, entry, detected, verdict, Some(text))
}

/// Turns a gate's verdict on a request aimed at `entry`, judged against
/// `detected`, into its outcome, queueing it for a human when it needs one.
fn gate_window<T: GatedTool>(
    context: &GateContext<'_>,
    entry: &WindowEntry,
    detected: DetectedStatus,
    verdict: Result<GateVerdict, DenyReason>,
    text: Option<&str>,
) -> GateOutcome<T> {
    let tool = T::TOOL;
    match verdict {
        Ok(GateVerdict::Allow) => return GateOutcome::Proceed(Permit::issue()),
        Ok(GateVerdict::NeedsApproval) => {}
        Err(reason) => return GateOutcome::Refused(deny_msg(tool, reason)),
    }

    queue_for_approval(
        context,
        ApprovalDraft {
            tool,
            requester: requester_for(context.registry, context.origin),
            target: Some(ApprovalTarget {
                window_id: entry.window_id().to_string(),
                // The id identifies the window unambiguously, so it stands in
                // for a name the registry no longer carries.
                name: window_info(context.registry, entry.window_id())
                    .map(|window| window.name)
                    .unwrap_or_else(|| entry.window_id().to_string()),
                status: detected.status(),
            }),
            text: text.map(str::to_string),
            room: None,
        },
    )
}

/// Runs a handoff that opens a window from a profile past the approval gate,
/// before anything starts: the envelope reaches the CLI the moment it does.
///
/// There is no window yet, so only the policy half of the gate applies, as
/// for [`gate_open`]. One decision both starts a process and hands it the
/// envelope, so it passes on its own only when the policy lets both
/// `open_window` and `send_handoff` through; otherwise it is asked for once.
/// `text` is what the approver reads: the lines naming what would be started,
/// then the envelope.
pub(super) fn gate_launched_handoff(
    context: &GateContext<'_>,
    text: &str,
) -> GateOutcome<LaunchHandoff> {
    let tool = LaunchHandoff::TOOL;
    let policy_snapshot = context.policy.snapshot();
    let mut needs_approval = false;
    for gated in [OpenWindow::TOOL, tool] {
        match control::policy_gate(
            live_policy::policy_source(&policy_snapshot),
            gated,
            context.origin,
            None,
        ) {
            Ok(GateVerdict::Allow) => {}
            Ok(GateVerdict::NeedsApproval) => needs_approval = true,
            Err(reason) => return GateOutcome::Refused(deny_msg(gated, reason)),
        }
    }
    if !needs_approval {
        return GateOutcome::Proceed(Permit::issue());
    }
    queue_for_approval(
        context,
        ApprovalDraft {
            tool,
            requester: requester_for(context.registry, context.origin),
            target: None,
            text: Some(text.to_string()),
            room: None,
        },
    )
}

/// Runs a request to open a window past the approval gate, reporting the
/// verdict rather than sending it.
///
/// There is no window yet, so only the policy half of the gate applies
/// ([`control::policy_gate`] with no target): the requester cannot be
/// addressing its own window. `description` is what the approver reads.
pub(super) fn gate_open(
    context: &GateContext<'_>,
    description: &OpenDescription,
) -> GateOutcome<OpenWindow> {
    let tool = OpenWindow::TOOL;
    let policy_snapshot = context.policy.snapshot();
    match control::policy_gate(
        live_policy::policy_source(&policy_snapshot),
        tool,
        context.origin,
        None,
    ) {
        Ok(GateVerdict::Allow) => return GateOutcome::Proceed(Permit::issue()),
        Ok(GateVerdict::NeedsApproval) => {}
        Err(reason) => return GateOutcome::Refused(deny_msg(tool, reason)),
    }
    queue_for_approval(
        context,
        ApprovalDraft {
            tool,
            requester: requester_for(context.registry, context.origin),
            target: None,
            text: Some(description.text.clone()),
            room: None,
        },
    )
}

/// Queues `draft` for a human decision. A body larger than an approval may
/// carry is refused rather than shown with its tail hidden.
fn queue_for_approval<T: GatedTool>(
    context: &GateContext<'_>,
    draft: ApprovalDraft,
) -> GateOutcome<T> {
    if draft
        .text
        .as_ref()
        .is_some_and(|text| text.len() > APPROVAL_TEXT_MAX_BYTES)
    {
        return GateOutcome::Refused(error_msg(
            "invalid_argument",
            "text exceeds the maximum size a request awaiting approval may carry",
        ));
    }

    let tool = draft.tool;
    let requester_kind = draft.requester.kind;
    match context
        .broker
        .submit(draft, context.conn_id, approval_limits(context.config))
    {
        Ok(ticket) => {
            tracing::info!(
                request_id = %ticket.request_id(),
                tool = tool.as_tool_name(),
                requester = ?requester_kind,
                "queued a destructive request for approval"
            );
            GateOutcome::Pending(ticket)
        }
        Err(SubmitError::NoApprover) => GateOutcome::Refused(deny_no_approver(tool)),
        Err(SubmitError::TooManyPending | SubmitError::TooManyPendingForConn) => {
            GateOutcome::Refused(error_msg(
                "resource_exhausted",
                "too many requests are already awaiting approval",
            ))
        }
    }
}

/// What a window open would start, as the lines an approver reads.
///
/// Every value is shown with its control characters escaped, so a command
/// argument or a path cannot forge a line of its own.
pub(super) struct OpenDescription {
    text: String,
}

/// Why a profile-launched window is opened, when it is for something.
pub(super) enum OpenPurpose<'a> {
    Handoff,
    PlanTask(&'a str),
}

impl OpenDescription {
    /// The lines an approver reads.
    pub(super) fn text(&self) -> &str {
        &self.text
    }

    /// A window that runs `argv` as given, each argument quoted where its
    /// boundaries would otherwise be unclear.
    pub(super) fn for_command(argv: &[String], cwd: Option<&str>) -> Self {
        let argv: Vec<String> = argv.iter().map(|arg| quote_argument(arg)).collect();
        Self::from_lines(format!("command: {}", argv.join(" ")), cwd, None)
    }

    /// A window launched from the launch profile `profile`.
    pub(super) fn for_profile(
        profile: &str,
        cwd: Option<&str>,
        purpose: Option<OpenPurpose<'_>>,
    ) -> Self {
        Self::from_lines(
            format!("profile: {}", escape_control(profile)),
            cwd,
            purpose,
        )
    }

    fn from_lines(subject: String, cwd: Option<&str>, purpose: Option<OpenPurpose<'_>>) -> Self {
        let mut lines = vec![subject];
        if let Some(cwd) = cwd {
            lines.push(format!("cwd: {}", escape_control(cwd)));
        }
        match purpose {
            Some(OpenPurpose::Handoff) => lines.push("for: handoff".to_string()),
            Some(OpenPurpose::PlanTask(task_id)) => {
                lines.push(format!("for: plan task \"{}\"", escape_control(task_id)));
            }
            None => {}
        }
        Self {
            text: lines.join("\n"),
        }
    }
}

/// `value` with each control character written as an escape sequence.
fn escape_control(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        if c.is_control() {
            escaped.extend(c.escape_default());
        } else {
            escaped.push(c);
        }
    }
    escaped
}

/// One command argument as an approver reads it: written as it is unless it
/// is empty or holds whitespace, `"` or `\`, in which case it is wrapped in
/// double quotes with `"` and `\` escaped.
///
/// Backslashes are escaped before control characters are, so a literal
/// `\n` and a newline never read the same.
fn quote_argument(arg: &str) -> String {
    let is_special = |c: char| c == '"' || c == '\\';
    let needs_quotes = arg.is_empty() || arg.chars().any(|c| c.is_whitespace() || is_special(c));
    if !needs_quotes {
        return escape_control(arg);
    }
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    for c in arg.chars() {
        if is_special(c) {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    escape_control(&quoted)
}

/// How a queued request was decided.
// Built once per request and consumed at once; boxing the answer would put an
// indirection in every caller's pattern and buy nothing.
#[allow(clippy::large_enum_variant)]
pub(super) enum Verdict<T: GatedTool> {
    /// The gate or an approver allowed it; the permit licenses it.
    Approved(Permit<T>),
    /// It was denied or timed out, carrying the answer the requester must get.
    Refused(DaemonMessage),
}

/// How a queued request to open a window was decided.
pub(super) type OpenVerdict = Verdict<OpenWindow>;

/// Waits for the decision on a queued request. `None` means the requester
/// left (`closed` was raised) first, so nobody is left to answer.
pub(super) async fn await_verdict<T: GatedTool>(
    mut ticket: Ticket,
    config: &DaemonConfig,
    closed: &mut watch::Receiver<bool>,
) -> Option<Verdict<T>> {
    let deadline = Duration::from_secs(config.approval_timeout_secs);
    match await_approval(&mut ticket, deadline, closed).await {
        ApprovalEnd::Approved => Some(Verdict::Approved(Permit::issue())),
        ApprovalEnd::Refused(message) => Some(Verdict::Refused(message)),
        ApprovalEnd::RequesterGone => None,
    }
}

/// The shape of the text `tool` will type, decided before approval: a
/// `send_message` body that is not exactly one line is written as it is, and
/// every other tool's text is judged against the composer.
fn send_message_write(tool: DestructiveTool, text: Option<&str>) -> ComposerWrite {
    let is_raw_message = tool == DestructiveTool::SendMessage
        && text.is_some_and(|text| single_line_body(text).is_none());
    if is_raw_message {
        ComposerWrite::Raw
    } else {
        ComposerWrite::Judged
    }
}

/// An approved request paired with the permit its execution runs under: the
/// arguments its handler already validated, plus the licence the recheck
/// issued.
pub(super) enum PermittedAction {
    SendText(String, Permit<SendMessage>),
    SendKeys(Vec<ControlKey>, Permit<SendKeys>),
    Interrupt(Permit<InterruptWindow>),
    Close(bool, Permit<CloseWindow>),
    SendHandoff {
        text: String,
        requester: Option<String>,
        slot: Option<HandoffSlot>,
        permit: Permit<SendHandoff>,
    },
}

/// What the recheck an approved request goes through decided.
pub(super) enum ApprovedOutcome {
    /// The request may run, carrying its licence.
    Permitted(PermittedAction),
    /// The request is refused, carrying the action back so whoever waited on
    /// it can give back what it holds before the refusal is sent.
    Denied(PendingAction, DenyReason),
}

/// Judges an approved request against the window's status now, licensing it
/// only if that status still allows it.
///
/// An approval decided against the old status is no licence to fire at the
/// new one, so the permit is issued by this recheck rather
/// than by the approval that preceded it.
pub(super) fn gate_approved(action: PendingAction, detected: DetectedStatus) -> ApprovedOutcome {
    // Only the status is rechecked: a window's composer access is fixed for
    // its lifetime, so the composer gate's pre-approval answer holds.
    if let Err(reason) = control::status_gate(action.tool(), detected) {
        return ApprovedOutcome::Denied(action, reason);
    }
    ApprovedOutcome::Permitted(match action {
        PendingAction::SendText(text) => PermittedAction::SendText(text, Permit::issue()),
        PendingAction::SendKeys(keys) => PermittedAction::SendKeys(keys, Permit::issue()),
        PendingAction::Interrupt => PermittedAction::Interrupt(Permit::issue()),
        PendingAction::Close(force) => PermittedAction::Close(force, Permit::issue()),
        PendingAction::SendHandoff {
            text,
            requester,
            slot,
        } => PermittedAction::SendHandoff {
            text,
            requester,
            slot,
            permit: Permit::issue(),
        },
    })
}
