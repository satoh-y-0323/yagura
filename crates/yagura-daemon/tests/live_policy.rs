#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `PolicyCell`: the daemon-wide cell a live approval policy lives in,
//! separate from `DaemonConfig`'s startup-time fields. Exercises the initial
//! snapshot against the three playbook states, per-tool and plan-review
//! writes (including duplicate and unknown `auto_approve` entries), a
//! snapshot's immutability across a later write, and the no-op write path
//! for a cell that never loaded anything to mutate.
//!
//! `IpcServer` wiring (routing a live request through the cell) is out of
//! scope here; it belongs to the IPC-layer tests once the cell is actually
//! read from the connection loop.

use std::path::PathBuf;
use std::sync::Arc;

use yagura_daemon::config::DaemonConfig;
use yagura_daemon::control::{
    ApprovalPolicy, ApprovalSection, DenyReason, DestructiveTool, GateVerdict, PolicySource,
    RequestOrigin, policy_gate,
};
use yagura_daemon::live_policy::{
    LivePolicy, LiveSource, PolicyCell, PolicyWriteGuard, policy_source,
};

/// Builds a policy that auto-approves exactly `tools`, mirroring
/// `control_gate.rs`'s fixture of the same name so both files pin the same
/// shape.
fn policy_approving(tools: &[&str]) -> ApprovalPolicy {
    ApprovalPolicy {
        approval: Some(ApprovalSection {
            auto_approve: tools.iter().map(|tool| (*tool).to_string()).collect(),
        }),
        limits: None,
        chat: None,
        plans: None,
    }
}

/// Builds a `DaemonConfig` carrying exactly the three fields a `PolicyCell`
/// is seeded from, everything else left at its default.
fn config_with(
    playbook: Option<PathBuf>,
    approval_policy: Option<ApprovalPolicy>,
    plan_approval_required: bool,
) -> DaemonConfig {
    DaemonConfig {
        playbook,
        approval_policy,
        plan_approval_required,
        ..DaemonConfig::default()
    }
}

/// Mirrors `ipc.rs`'s own (private) `policy_source_from_config`: the
/// pre-cell logic the initial snapshot is checked against.
fn expected_policy_source(config: &DaemonConfig) -> PolicySource<'_> {
    match config.approval_policy.as_ref() {
        Some(policy) => PolicySource::Loaded(policy),
        None if config.playbook.is_some() => PolicySource::Failed,
        None => PolicySource::NotConfigured,
    }
}

/// `policy_gate`'s verdict for `tool` under a fixed, external, unidentified
/// origin and no target window -- the same call shape for every comparison
/// in this file, so two `PolicySource`s are compared by effect rather than
/// by field equality (`ApprovalPolicy` has none).
fn gate(policy: PolicySource<'_>, tool: DestructiveTool) -> Result<GateVerdict, DenyReason> {
    policy_gate(policy, tool, &RequestOrigin::External, None)
}

/// [`gate`]'s verdict for every tool in `DestructiveTool::ALL`, in that
/// order.
fn verdicts(policy: PolicySource<'_>) -> Vec<Result<GateVerdict, DenyReason>> {
    DestructiveTool::ALL
        .iter()
        .map(|tool| gate(policy, *tool))
        .collect()
}

#[test]
fn initial_snapshot_mirrors_the_pre_cell_policy_source_and_carries_the_plan_flag() {
    let not_configured = config_with(None, None, true);
    let failed = config_with(Some(PathBuf::from("unused.toml")), None, false);
    let loaded = config_with(
        Some(PathBuf::from("unused.toml")),
        Some(policy_approving(&["send_chat"])),
        true,
    );

    for config in [&not_configured, &failed, &loaded] {
        let cell: PolicyCell = PolicyCell::from_config(config);
        let snapshot: Arc<LivePolicy> = cell.snapshot();

        match (&snapshot.source, expected_policy_source(config)) {
            (LiveSource::NotConfigured, PolicySource::NotConfigured)
            | (LiveSource::Failed, PolicySource::Failed)
            | (LiveSource::Loaded(_), PolicySource::Loaded(_)) => {}
            (actual, expected) => panic!(
                "the cell's initial LiveSource must mirror policy_source_from_config's \
                 PolicySource; got {actual:?}, expected the variant matching {expected:?}"
            ),
        }

        assert_eq!(
            verdicts(policy_source(&snapshot)),
            verdicts(expected_policy_source(config)),
            "policy_gate's verdict for every DestructiveTool must be unchanged by routing \
             through the cell"
        );

        assert_eq!(
            snapshot.plans_require_approval, config.plan_approval_required,
            "the cell must carry the startup plan_approval_required value forward unchanged"
        );
    }
}

#[tokio::test]
async fn set_auto_approve_toggles_only_the_named_tool_and_purges_duplicate_entries() {
    let config = config_with(
        Some(PathBuf::from("unused.toml")),
        Some(policy_approving(&[
            "send_chat",
            "send_keys",
            "send_keys",
            "typo_tool",
        ])),
        true,
    );
    let cell: PolicyCell = PolicyCell::from_config(&config);

    let before: Vec<Result<GateVerdict, DenyReason>> = DestructiveTool::ALL
        .iter()
        .map(|tool| gate(policy_source(&cell.snapshot()), *tool))
        .collect();
    let index_of = |tool: DestructiveTool| {
        DestructiveTool::ALL
            .iter()
            .position(|candidate| *candidate == tool)
            .expect("tool must be in ALL")
    };
    assert_eq!(
        before[index_of(DestructiveTool::SendChat)],
        Ok(GateVerdict::Allow)
    );
    assert_eq!(
        before[index_of(DestructiveTool::SendMessage)],
        Ok(GateVerdict::NeedsApproval)
    );

    {
        let mut guard: PolicyWriteGuard<'_> = cell.lock_writes().await;
        let changed: bool = guard.set_auto_approve(DestructiveTool::SendMessage, true);
        assert!(
            changed,
            "turning a tool on that was off must report a change"
        );
    }
    let after_on: Arc<LivePolicy> = cell.snapshot();
    assert_eq!(
        gate(policy_source(&after_on), DestructiveTool::SendMessage),
        Ok(GateVerdict::Allow)
    );
    for tool in DestructiveTool::ALL {
        if tool == DestructiveTool::SendMessage {
            continue;
        }
        assert_eq!(
            gate(policy_source(&after_on), tool),
            before[index_of(tool)],
            "toggling send_message must not move any other tool's verdict, tool={tool:?}"
        );
    }

    {
        let mut guard: PolicyWriteGuard<'_> = cell.lock_writes().await;
        let changed_again: bool = guard.set_auto_approve(DestructiveTool::SendMessage, true);
        assert!(
            !changed_again,
            "an already-satisfied request must report no change"
        );
    }
    assert_eq!(
        gate(
            policy_source(&cell.snapshot()),
            DestructiveTool::SendMessage
        ),
        Ok(GateVerdict::Allow),
        "a no-op write must leave the snapshot's content exactly as it was"
    );

    {
        let mut guard: PolicyWriteGuard<'_> = cell.lock_writes().await;
        let changed: bool = guard.set_auto_approve(DestructiveTool::SendKeys, false);
        assert!(
            changed,
            "turning off a duplicated entry must still report a change"
        );
    }
    let after_off: Arc<LivePolicy> = cell.snapshot();
    assert_eq!(
        gate(policy_source(&after_off), DestructiveTool::SendKeys),
        Ok(GateVerdict::NeedsApproval),
        "every duplicate entry for the same tool must be purged by one off toggle"
    );
    let LiveSource::Loaded(policy) = &after_off.source else {
        panic!("the policy must still be Loaded after a write");
    };
    let names: &Vec<String> = &policy
        .approval
        .as_ref()
        .expect("the approval section must still be present")
        .auto_approve;
    assert!(
        !names.iter().any(|name| name == "send_keys"),
        "no send_keys entry, duplicated or not, may remain: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "typo_tool"),
        "an unrelated, unknown tool name must not be purged as a side effect: {names:?}"
    );
}

#[tokio::test]
async fn set_plan_approval_required_reports_change_and_updates_the_snapshot() {
    let config = config_with(None, None, true);
    let cell: PolicyCell = PolicyCell::from_config(&config);
    assert!(cell.snapshot().plans_require_approval);

    {
        let mut guard: PolicyWriteGuard<'_> = cell.lock_writes().await;
        let changed: bool = guard.set_plan_approval_required(false);
        assert!(
            changed,
            "turning a flag off that was on must report a change"
        );
    }
    assert!(!cell.snapshot().plans_require_approval);

    {
        let mut guard: PolicyWriteGuard<'_> = cell.lock_writes().await;
        let changed_again: bool = guard.set_plan_approval_required(false);
        assert!(
            !changed_again,
            "an already-satisfied request must report no change"
        );
    }
    assert!(
        !cell.snapshot().plans_require_approval,
        "a no-op write must leave the snapshot's content exactly as it was"
    );
}

#[tokio::test]
async fn a_snapshot_taken_before_a_write_keeps_reading_the_old_value() {
    let config = config_with(
        Some(PathBuf::from("unused.toml")),
        Some(policy_approving(&[])),
        true,
    );
    let cell: PolicyCell = PolicyCell::from_config(&config);
    let before: Arc<LivePolicy> = cell.snapshot();
    assert!(before.plans_require_approval);

    {
        let mut guard: PolicyWriteGuard<'_> = cell.lock_writes().await;
        guard.set_plan_approval_required(false);
    }

    assert!(
        before.plans_require_approval,
        "a snapshot taken before the write must keep reading the old value, since it holds its \
         own Arc rather than following the cell's current one"
    );
    assert!(!cell.snapshot().plans_require_approval);
}

#[tokio::test]
async fn writes_are_rejected_without_effect_when_nothing_is_loaded() {
    for config in [
        config_with(None, None, true),
        config_with(Some(PathBuf::from("unused.toml")), None, true),
    ] {
        let cell: PolicyCell = PolicyCell::from_config(&config);
        let before_is_loaded = matches!(cell.snapshot().source, LiveSource::Loaded(_));
        assert!(
            !before_is_loaded,
            "this fixture must start as NotConfigured or Failed, never Loaded"
        );
        let expected_verdict: Result<GateVerdict, DenyReason> = gate(
            expected_policy_source(&config),
            DestructiveTool::SendMessage,
        );

        {
            let mut guard: PolicyWriteGuard<'_> = cell.lock_writes().await;
            let changed: bool = guard.set_auto_approve(DestructiveTool::SendMessage, true);
            assert!(
                !changed,
                "a cell with nothing loaded must not report a change"
            );
        }

        let after: Arc<LivePolicy> = cell.snapshot();
        assert!(
            !matches!(after.source, LiveSource::Loaded(_)),
            "a rejected write must never turn NotConfigured/Failed into Loaded"
        );
        assert_eq!(
            gate(policy_source(&after), DestructiveTool::SendMessage),
            expected_verdict,
            "the gate's verdict must be unaffected by the rejected write"
        );
    }
}
