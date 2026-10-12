//! IPC handlers for `get_approval_settings`, `set_auto_approve` and
//! `set_plan_approval_required`: the approver-only view and switches of the
//! daemon's live approval policy.
//!
//! A switch makes two separate judgements. Whether the playbook file is
//! rewritten is decided by [`playbook_rewrite`] from the file's own current
//! value; whether the daemon's value changes (and the switch is logged) is
//! decided here from the [`PolicyCell`]'s value. The two may disagree after a
//! manual edit of the file, and a switch must still reach the daemon then.

use std::path::PathBuf;
use std::sync::Arc;

use yagura_protocol::{ApprovalSettingsInfo, DaemonMessage, PlaybookState, ToolAutoApprove};

use crate::config::DaemonConfig;
use crate::control::{DestructiveTool, RequestOrigin};
use crate::live_policy::{LivePolicy, LiveSource, PolicyCell, PolicyWriteGuard};
use crate::playbook_rewrite::{self, PlaybookRewriteError, PlaybookWrite};

use super::plan::{auth_failed_msg, unsupported_msg};
use super::{ConnectionState, Outbound, error_msg};

/// Answers `get_approval_settings`, approver-only, with the daemon's current
/// value.
pub(super) fn handle_get_approval_settings(
    policy: &PolicyCell,
    outbound: &Arc<Outbound>,
    state: &ConnectionState,
) {
    if !matches!(state.origin, RequestOrigin::Approver) {
        outbound.reply(&auth_failed_msg());
        return;
    }
    outbound.reply(&DaemonMessage::ApprovalSettings {
        settings: settings_info(&policy.snapshot()),
    });
}

/// Answers `set_auto_approve`, approver-only: turns one destructive tool's
/// auto-approval on or off in both the playbook file and the daemon.
pub(super) async fn handle_set_auto_approve(
    config: &DaemonConfig,
    policy: &PolicyCell,
    outbound: &Arc<Outbound>,
    state: &ConnectionState,
    tool: String,
    auto_approve: bool,
) {
    if !matches!(state.origin, RequestOrigin::Approver) {
        outbound.reply(&auth_failed_msg());
        return;
    }
    let Some(tool) = DestructiveTool::ALL
        .into_iter()
        .find(|known| known.as_tool_name() == tool)
    else {
        outbound.reply(&error_msg("invalid_argument", "unknown tool name"));
        return;
    };
    let change = Change::AutoApprove { tool, auto_approve };
    match apply_change(config, policy, change).await {
        Ok(settings) => outbound.reply(&DaemonMessage::AutoApproveSet { settings }),
        Err(refusal) => outbound.reply(&refusal.to_message()),
    }
}

/// Answers `set_plan_approval_required`, approver-only: turns the
/// plan-review requirement on or off in both the playbook file and the
/// daemon.
pub(super) async fn handle_set_plan_approval_required(
    config: &DaemonConfig,
    policy: &PolicyCell,
    outbound: &Arc<Outbound>,
    state: &ConnectionState,
    required: bool,
) {
    if !matches!(state.origin, RequestOrigin::Approver) {
        outbound.reply(&auth_failed_msg());
        return;
    }
    let change = Change::PlanApprovalRequired { required };
    match apply_change(config, policy, change).await {
        Ok(settings) => outbound.reply(&DaemonMessage::PlanApprovalRequiredSet { settings }),
        Err(refusal) => outbound.reply(&refusal.to_message()),
    }
}

/// The one setting a switch request changes.
#[derive(Debug, Clone, Copy)]
enum Change {
    AutoApprove {
        tool: DestructiveTool,
        auto_approve: bool,
    },
    PlanApprovalRequired {
        required: bool,
    },
}

impl Change {
    fn write_file(self, path: &std::path::Path) -> Result<PlaybookWrite, PlaybookRewriteError> {
        match self {
            Self::AutoApprove { tool, auto_approve } => {
                playbook_rewrite::set_auto_approve(path, tool, auto_approve)
            }
            Self::PlanApprovalRequired { required } => {
                playbook_rewrite::set_plan_approval_required(path, required)
            }
        }
    }

    /// Installs this one setting into the daemon's value, logging the switch
    /// only when that value actually changed.
    fn apply_to_daemon(self, guard: &mut PolicyWriteGuard<'_>) {
        match self {
            Self::AutoApprove { tool, auto_approve } => {
                if guard.set_auto_approve(tool, auto_approve) {
                    tracing::info!(
                        tool = tool.as_tool_name(),
                        auto_approve,
                        "approval setting changed"
                    );
                }
            }
            Self::PlanApprovalRequired { required } => {
                if guard.set_plan_approval_required(required) {
                    tracing::info!(
                        plan_approval_required = required,
                        "approval setting changed"
                    );
                }
            }
        }
    }
}

/// Why a switch was not carried out, each with its fixed wire reply.
#[derive(Debug, Clone, Copy)]
enum Refusal {
    /// No playbook file backs the daemon's policy.
    NotConfigured,
    /// The playbook failed to load at boot.
    FailedAtBoot,
    /// The file on disk no longer reads as a playbook.
    Unreadable,
    /// The file could not be rewritten; the detail went to the log.
    WriteFailed,
}

impl Refusal {
    fn to_message(self) -> DaemonMessage {
        match self {
            Self::NotConfigured => unsupported_msg(),
            Self::FailedAtBoot => error_msg(
                "invalid_state",
                "the playbook did not load when the daemon started",
            ),
            Self::Unreadable => {
                error_msg("invalid_state", "the playbook on disk could not be read")
            }
            Self::WriteFailed => error_msg("internal", "the playbook could not be written"),
        }
    }
}

/// Carries out one switch: the file first, then the daemon's value, so a
/// failed write never leaves the daemon ahead of the file. Holding the
/// cell's write guard across both keeps at most one rewrite (and so one
/// blocking thread) in flight.
async fn apply_change(
    config: &DaemonConfig,
    policy: &PolicyCell,
    change: Change,
) -> Result<ApprovalSettingsInfo, Refusal> {
    let path = writable_playbook_path(config, &policy.snapshot())?;
    let mut guard = policy.lock_writes().await;
    // A blocking thread does not inherit the caller's span; carrying it keeps
    // the rewrite's own log lines attributed to this request.
    let span = tracing::Span::current();
    let written =
        tokio::task::spawn_blocking(move || span.in_scope(|| change.write_file(&path))).await;
    match written {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => return Err(rewrite_refusal(&err)),
        Err(join_err) => {
            tracing::warn!(error = %join_err, "approval setting rewrite task failed");
            return Err(Refusal::WriteFailed);
        }
    }
    change.apply_to_daemon(&mut guard);
    drop(guard);
    Ok(settings_info(&policy.snapshot()))
}

/// The playbook file a switch may rewrite, or why the daemon's playbook
/// state admits no rewrite.
fn writable_playbook_path(config: &DaemonConfig, current: &LivePolicy) -> Result<PathBuf, Refusal> {
    match (&current.source, config.playbook.as_ref()) {
        (LiveSource::Loaded(_), Some(path)) => Ok(path.clone()),
        // A policy loaded without a file behind it has nowhere to persist a
        // switch, which is the same standing as having no playbook at all.
        (LiveSource::Loaded(_) | LiveSource::NotConfigured, _) => Err(Refusal::NotConfigured),
        // Taking a since-repaired file in by rewriting it would leave chat,
        // stopped at boot, disagreeing with it; only a restart recovers.
        (LiveSource::Failed, _) => Err(Refusal::FailedAtBoot),
    }
}

fn rewrite_refusal(err: &PlaybookRewriteError) -> Refusal {
    match err {
        PlaybookRewriteError::Unreadable => Refusal::Unreadable,
        PlaybookRewriteError::RewriteFailed | PlaybookRewriteError::Io(_) => {
            tracing::warn!(error = %err, "approval setting could not be written to the playbook");
            Refusal::WriteFailed
        }
    }
}

/// The wire view of one snapshot, every destructive tool in
/// [`DestructiveTool::ALL`]'s order. Only a loaded policy auto-approves
/// anything: with no playbook every tool waits for approval, and with a
/// failed one every tool is refused.
fn settings_info(current: &LivePolicy) -> ApprovalSettingsInfo {
    let (playbook, loaded) = match &current.source {
        LiveSource::Loaded(policy) => (PlaybookState::Loaded, Some(policy)),
        LiveSource::NotConfigured => (PlaybookState::NotConfigured, None),
        LiveSource::Failed => (PlaybookState::Failed, None),
    };
    let tools = DestructiveTool::ALL
        .into_iter()
        .map(|tool| ToolAutoApprove {
            tool: tool.as_tool_name().to_string(),
            auto_approve: loaded.is_some_and(|policy| policy.allows(tool)),
        })
        .collect();
    ApprovalSettingsInfo {
        playbook,
        tools,
        plans_require_approval: current.plans_require_approval,
    }
}
