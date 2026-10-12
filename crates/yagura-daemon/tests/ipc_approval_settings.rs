#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the daemon IPC surface for reading and switching the
//! approval settings themselves (as opposed to `ipc_approval_live.rs`, which
//! measures a switch's effect on a later decision).
//!
//! Covers who may call the three requests, what a request answers when the
//! playbook cannot be used to satisfy it, what a read reports across the
//! playbook's states, that a write never lets an untouched setting drift
//! from a manual edit of the file, that a setting the file and the daemon
//! disagree on can still be switched in either direction, and that a
//! successful switch is logged exactly once while a request that already
//! matches is not.
//!
//! Every fixture boots the daemon from a real playbook file (see
//! [`common::live_playbook_config`]), the same discipline
//! `ipc_approval_live.rs` documents.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::{
    Harness, approver_base_config, detection_config, expect_error, identify, live_playbook_config,
    read_approver_reply, read_response, register_approver, send, unique_temp_dir, write_profile,
};
use tracing_test::traced_test;
use yagura_daemon::DaemonConfig;
use yagura_daemon::control::DestructiveTool;
use yagura_daemon::plan_permit::LockTable;
use yagura_protocol::{
    ApprovalRequest, ApprovalSettingsInfo, ClientMessage, ControlKey, DaemonMessage, PlanInfo,
    PlanTaskSpec, PlaybookState, ToolAutoApprove,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::ready_child_profile_source;

/// Upper bound for a reply that is expected to arrive.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a case that must observe "no further matching log line appears"
/// waits before checking.
const NEGATIVE_LOG_OBSERVATION: Duration = Duration::from_millis(500);

const NO_AUTO_APPROVE_PLAYBOOK: &str = "[approval]\nauto_approve = []\n";

/// A file that does not parse as a playbook: used both as the boot-time
/// fixture for a `Failed` source and to corrupt an already-loaded one after
/// boot.
const BROKEN_PLAYBOOK_SOURCE: &str = "not = [valid\n";

// --- shared steps -------------------------------------------------------------

/// Opens an idle window and identifies a fresh connection as it.
async fn identified_window(h: &Harness) -> (Connection, String) {
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &window_id).await;
    (client, window_id)
}

async fn call_get_approval_settings(client: &mut Connection) -> DaemonMessage {
    send(client, &ClientMessage::GetApprovalSettings {}).await;
    read_approver_reply(client, REPLY_TIMEOUT).await
}

async fn call_set_auto_approve(
    client: &mut Connection,
    tool: &str,
    auto_approve: bool,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::SetAutoApprove {
            tool: tool.to_string(),
            auto_approve,
        },
    )
    .await;
    read_approver_reply(client, REPLY_TIMEOUT).await
}

async fn call_set_plan_approval_required(client: &mut Connection, required: bool) -> DaemonMessage {
    send(client, &ClientMessage::SetPlanApprovalRequired { required }).await;
    read_approver_reply(client, REPLY_TIMEOUT).await
}

fn expect_approval_settings(reply: DaemonMessage) -> ApprovalSettingsInfo {
    match reply {
        DaemonMessage::ApprovalSettings { settings } => settings,
        other => panic!("expected approval_settings, got {other:?}"),
    }
}

fn expect_auto_approve_set(reply: DaemonMessage) -> ApprovalSettingsInfo {
    match reply {
        DaemonMessage::AutoApproveSet { settings } => settings,
        other => panic!("expected auto_approve_set, got {other:?}"),
    }
}

fn expect_plan_approval_required_set(reply: DaemonMessage) -> ApprovalSettingsInfo {
    match reply {
        DaemonMessage::PlanApprovalRequiredSet { settings } => settings,
        other => panic!("expected plan_approval_required_set, got {other:?}"),
    }
}

/// The `auto_approve` value `settings` reports for `tool`, or `None` when the
/// tool is not listed at all.
fn reported_auto_approve(settings: &ApprovalSettingsInfo, tool: &str) -> Option<bool> {
    settings
        .tools
        .iter()
        .find(|entry: &&ToolAutoApprove| entry.tool == tool)
        .map(|entry: &ToolAutoApprove| entry.auto_approve)
}

fn tool_names(tools: &[ToolAutoApprove]) -> Vec<&str> {
    tools
        .iter()
        .map(|entry: &ToolAutoApprove| entry.tool.as_str())
        .collect()
}

fn expected_tool_order() -> Vec<&'static str> {
    DestructiveTool::ALL
        .iter()
        .map(|tool: &DestructiveTool| tool.as_tool_name())
        .collect()
}

/// Makes the next rewrite of `path` fail on every OS, or lets it through
/// again. A read-only file only blocks the replacing rename on Windows; on
/// Unix the rename succeeds whatever the file's mode, so the temporary file
/// the rewrite writes next to it is occupied by a directory instead.
fn block_rewrite(path: &Path, blocked: bool) {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("the playbook path must end in a UTF-8 file name");
    let temp_path = path.with_file_name(format!(".{file_name}.tmp-{}", std::process::id()));
    if blocked {
        fs::create_dir(&temp_path)
            .unwrap_or_else(|err| panic!("block {}: {err}", temp_path.display()));
    } else {
        fs::remove_dir(&temp_path)
            .unwrap_or_else(|err| panic!("unblock {}: {err}", temp_path.display()));
    }
}

/// A config whose playbook path exists and carries a fixture that does not
/// parse, with `approval_policy` left `None` -- the same shape the daemon's
/// own boot path produces when the file it was told to load could not be
/// read.
fn failed_at_boot_config(dir: &Path, base: DaemonConfig) -> (DaemonConfig, PathBuf) {
    let path = dir.join("playbook.toml");
    fs::write(&path, BROKEN_PLAYBOOK_SOURCE).expect("write the broken playbook fixture");
    let mut config = base;
    config.playbook = Some(path.clone());
    config.approval_policy = None;
    config.plan_approval_required = false;
    (config, path)
}

async fn deny_and_confirm(
    approver: &mut Connection,
    requester: &mut Connection,
    request: &ApprovalRequest,
) {
    send(
        approver,
        &ClientMessage::ApprovalDecision {
            request_id: request.request_id.clone(),
            approved: false,
        },
    )
    .await;
    let reply: DaemonMessage = read_approver_reply(approver, REPLY_TIMEOUT).await;
    match reply {
        DaemonMessage::ApprovalResolved { request_id } => {
            assert_eq!(request_id, request.request_id);
        }
        other => panic!("expected approval_resolved, got {other:?}"),
    }
    let reply: DaemonMessage = read_response(requester, REPLY_TIMEOUT).await;
    expect_error(reply, "approval_denied");
}

// =============================================================================
// Permission: all three requests, from a window-identified connection and
// from an unidentified one (the MCP bridge's own standing).
// =============================================================================

async fn expect_all_three_requests_auth_failed(client: &mut Connection) {
    let reply: DaemonMessage = call_get_approval_settings(client).await;
    expect_error(reply, "auth_failed");
    let reply: DaemonMessage = call_set_auto_approve(client, "send_message", true).await;
    expect_error(reply, "auth_failed");
    let reply: DaemonMessage = call_set_plan_approval_required(client, false).await;
    expect_error(reply, "auth_failed");
}

#[tokio::test]
async fn the_three_requests_from_a_window_identified_connection_are_auth_failed_and_change_nothing()
{
    let dir = unique_temp_dir("settings-auth-window");
    let config: DaemonConfig = live_playbook_config(
        &dir,
        detection_config(&dir, approver_base_config()),
        NO_AUTO_APPROVE_PLAYBOOK,
    );
    let path: PathBuf = config.playbook.clone().expect("playbook path must be set");
    let h = Harness::start("settings-auth-window", config).await;
    let (mut client, _window_id) = identified_window(&h).await;
    let before: Vec<u8> = fs::read(&path).expect("read playbook before");

    expect_all_three_requests_auth_failed(&mut client).await;

    let after: Vec<u8> = fs::read(&path).expect("read playbook after");
    assert_eq!(
        before, after,
        "an auth_failed attempt must leave the playbook file byte-for-byte unchanged"
    );

    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let reply: DaemonMessage = call_get_approval_settings(&mut approver).await;
    let settings: ApprovalSettingsInfo = expect_approval_settings(reply);
    assert_eq!(
        reported_auto_approve(&settings, "send_message"),
        Some(false),
        "a refused switch must not have changed the daemon's value"
    );
    assert!(
        settings.plans_require_approval,
        "a refused switch must not have changed the daemon's plan-review value"
    );

    // The window's own evidence: a send_message request still waits for
    // approval, proving the refused switch never actually took effect.
    let mut target_client = h.connect().await;
    let target_window: String = h.open_idle_window(&mut target_client).await;
    send(
        &mut client,
        &ClientMessage::SendText {
            window_id: target_window.clone(),
            text: "still gated".to_string(),
        },
    )
    .await;
    let request: ApprovalRequest = match read_approver_reply(&mut approver, REPLY_TIMEOUT).await {
        DaemonMessage::ApprovalRequested { request } => request,
        other => panic!("expected approval_requested, got {other:?}"),
    };
    assert_eq!(request.tool, "send_message");
    deny_and_confirm(&mut approver, &mut client, &request).await;

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn the_three_requests_from_an_unidentified_connection_are_auth_failed_and_change_nothing() {
    let dir = unique_temp_dir("settings-auth-external");
    let config: DaemonConfig = live_playbook_config(
        &dir,
        detection_config(&dir, approver_base_config()),
        NO_AUTO_APPROVE_PLAYBOOK,
    );
    let path: PathBuf = config.playbook.clone().expect("playbook path must be set");
    let h = Harness::start("settings-auth-external", config).await;
    let mut external = h.connect().await;
    let before: Vec<u8> = fs::read(&path).expect("read playbook before");

    expect_all_three_requests_auth_failed(&mut external).await;

    let after: Vec<u8> = fs::read(&path).expect("read playbook after");
    assert_eq!(
        before, after,
        "an auth_failed attempt must leave the playbook file byte-for-byte unchanged"
    );

    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let reply: DaemonMessage = call_get_approval_settings(&mut approver).await;
    let settings: ApprovalSettingsInfo = expect_approval_settings(reply);
    assert_eq!(
        reported_auto_approve(&settings, "send_message"),
        Some(false)
    );
    assert!(settings.plans_require_approval);

    let (mut client, _own_window) = identified_window(&h).await;
    let target_window: String = h.open_idle_window(&mut client).await;
    send(
        &mut client,
        &ClientMessage::CloseWindow {
            window_id: target_window,
            force: None,
        },
    )
    .await;
    let request: ApprovalRequest = match read_approver_reply(&mut approver, REPLY_TIMEOUT).await {
        DaemonMessage::ApprovalRequested { request } => request,
        other => panic!("expected approval_requested, got {other:?}"),
    };
    assert_eq!(request.tool, "close_window");
    deny_and_confirm(&mut approver, &mut client, &request).await;

    h.registry.terminate_all().await;
}

// =============================================================================
// States the playbook rewrite cannot proceed from.
// =============================================================================

#[tokio::test]
async fn set_auto_approve_with_no_playbook_configured_is_unsupported_and_changes_nothing() {
    let config = DaemonConfig {
        playbook: None,
        approval_policy: None,
        ..approver_base_config()
    };
    let h = Harness::start("settings-notconfigured-auto", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let reply: DaemonMessage = call_set_auto_approve(&mut approver, "send_keys", true).await;
    expect_error(reply, "unsupported");

    let reply: DaemonMessage = call_get_approval_settings(&mut approver).await;
    let settings: ApprovalSettingsInfo = expect_approval_settings(reply);
    assert_eq!(settings.playbook, PlaybookState::NotConfigured);
    assert_eq!(reported_auto_approve(&settings, "send_keys"), Some(false));
}

#[tokio::test]
async fn set_plan_approval_required_with_no_playbook_configured_is_unsupported_and_changes_nothing()
{
    let config = DaemonConfig {
        playbook: None,
        approval_policy: None,
        plan_approval_required: true,
        ..approver_base_config()
    };
    let h = Harness::start("settings-notconfigured-plan", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let reply: DaemonMessage = call_set_plan_approval_required(&mut approver, false).await;
    expect_error(reply, "unsupported");

    let reply: DaemonMessage = call_get_approval_settings(&mut approver).await;
    let settings: ApprovalSettingsInfo = expect_approval_settings(reply);
    assert_eq!(settings.playbook, PlaybookState::NotConfigured);
    assert!(settings.plans_require_approval);
}

#[tokio::test]
async fn set_auto_approve_when_the_playbook_failed_to_load_at_boot_is_invalid_state_and_changes_nothing()
 {
    let dir = unique_temp_dir("settings-failed-auto");
    let (config, path) = failed_at_boot_config(&dir, approver_base_config());
    let h = Harness::start("settings-failed-auto", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let before: Vec<u8> = fs::read(&path).expect("read before");

    let reply: DaemonMessage = call_set_auto_approve(&mut approver, "send_keys", true).await;
    let message: String = expect_error(reply, "invalid_state");
    assert_eq!(message, "the playbook did not load when the daemon started");

    let after: Vec<u8> = fs::read(&path).expect("read after");
    assert_eq!(before, after);

    let reply: DaemonMessage = call_get_approval_settings(&mut approver).await;
    let settings: ApprovalSettingsInfo = expect_approval_settings(reply);
    assert_eq!(settings.playbook, PlaybookState::Failed);
    assert_eq!(reported_auto_approve(&settings, "send_keys"), Some(false));
}

#[tokio::test]
async fn set_plan_approval_required_when_the_playbook_failed_to_load_at_boot_is_invalid_state_and_changes_nothing()
 {
    let dir = unique_temp_dir("settings-failed-plan");
    let (config, path) = failed_at_boot_config(&dir, approver_base_config());
    let h = Harness::start("settings-failed-plan", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let before: Vec<u8> = fs::read(&path).expect("read before");

    let reply: DaemonMessage = call_set_plan_approval_required(&mut approver, true).await;
    let message: String = expect_error(reply, "invalid_state");
    assert_eq!(message, "the playbook did not load when the daemon started");

    let after: Vec<u8> = fs::read(&path).expect("read after");
    assert_eq!(before, after);

    let reply: DaemonMessage = call_get_approval_settings(&mut approver).await;
    let settings: ApprovalSettingsInfo = expect_approval_settings(reply);
    assert!(!settings.plans_require_approval);
}

#[tokio::test]
async fn set_auto_approve_after_the_playbook_is_corrupted_post_boot_is_invalid_state_and_changes_nothing()
 {
    let dir = unique_temp_dir("settings-unreadable-auto");
    let config: DaemonConfig = live_playbook_config(
        &dir,
        detection_config(&dir, approver_base_config()),
        NO_AUTO_APPROVE_PLAYBOOK,
    );
    let path: PathBuf = config.playbook.clone().expect("playbook path must be set");
    let h = Harness::start("settings-unreadable-auto", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    fs::write(&path, BROKEN_PLAYBOOK_SOURCE).expect("corrupt the file after boot");
    let before: Vec<u8> = fs::read(&path).expect("read before");

    let reply: DaemonMessage = call_set_auto_approve(&mut approver, "send_keys", true).await;
    let message: String = expect_error(reply, "invalid_state");
    assert_eq!(message, "the playbook on disk could not be read");

    let after: Vec<u8> = fs::read(&path).expect("read after");
    assert_eq!(
        before, after,
        "a refused rewrite must leave the corrupted file exactly as it was"
    );

    let reply: DaemonMessage = call_get_approval_settings(&mut approver).await;
    let settings: ApprovalSettingsInfo = expect_approval_settings(reply);
    assert_eq!(
        settings.playbook,
        PlaybookState::Loaded,
        "the daemon's own value must still reflect the boot-time load, unaffected by the later \
         corruption"
    );
    assert_eq!(reported_auto_approve(&settings, "send_keys"), Some(false));
}

#[tokio::test]
async fn set_auto_approve_that_cannot_write_the_playbook_is_internal_and_changes_nothing_until_the_write_can_go_through()
 {
    let dir = unique_temp_dir("settings-readonly-auto");
    let config: DaemonConfig = live_playbook_config(
        &dir,
        detection_config(&dir, approver_base_config()),
        NO_AUTO_APPROVE_PLAYBOOK,
    );
    let path: PathBuf = config.playbook.clone().expect("playbook path must be set");
    let h = Harness::start("settings-readonly-auto", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    block_rewrite(&path, true);
    let before: Vec<u8> = fs::read(&path).expect("read before");

    let reply: DaemonMessage = call_set_auto_approve(&mut approver, "send_keys", true).await;
    let message: String = expect_error(reply, "internal");
    assert_eq!(message, "the playbook could not be written");

    let after: Vec<u8> = fs::read(&path).expect("read after");
    assert_eq!(
        before, after,
        "a write failure must leave the file byte-for-byte unchanged"
    );

    let reply: DaemonMessage = call_get_approval_settings(&mut approver).await;
    let settings: ApprovalSettingsInfo = expect_approval_settings(reply);
    assert_eq!(
        reported_auto_approve(&settings, "send_keys"),
        Some(false),
        "the daemon's own value must be unaffected by a write failure"
    );

    // Control: the same request, the same fixture, succeeds once the write is
    // able to go through again.
    block_rewrite(&path, false);
    let reply: DaemonMessage = call_set_auto_approve(&mut approver, "send_keys", true).await;
    let settings: ApprovalSettingsInfo = expect_auto_approve_set(reply);
    assert_eq!(reported_auto_approve(&settings, "send_keys"), Some(true));
    let after_control: Vec<u8> = fs::read(&path).expect("read after control");
    assert_ne!(
        before, after_control,
        "once writable, the same request must actually rewrite the file"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn set_auto_approve_with_an_unknown_tool_name_is_invalid_argument_and_changes_nothing() {
    let dir = unique_temp_dir("settings-unknown-tool");
    let config: DaemonConfig = live_playbook_config(
        &dir,
        detection_config(&dir, approver_base_config()),
        NO_AUTO_APPROVE_PLAYBOOK,
    );
    let path: PathBuf = config.playbook.clone().expect("playbook path must be set");
    let h = Harness::start("settings-unknown-tool", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let before: Vec<u8> = fs::read(&path).expect("read before");

    let reply: DaemonMessage =
        call_set_auto_approve(&mut approver, "does-not-exist-anywhere", true).await;
    expect_error(reply, "invalid_argument");

    let after: Vec<u8> = fs::read(&path).expect("read after");
    assert_eq!(before, after);

    let reply: DaemonMessage = call_get_approval_settings(&mut approver).await;
    let settings: ApprovalSettingsInfo = expect_approval_settings(reply);
    assert!(
        settings
            .tools
            .iter()
            .all(|entry: &ToolAutoApprove| !entry.auto_approve)
    );
}

// =============================================================================
// A read reports the daemon's current value across every playbook state.
// =============================================================================

#[tokio::test]
async fn get_approval_settings_reports_the_daemons_current_value_across_playbook_states() {
    // Loaded.
    let dir = unique_temp_dir("settings-read-loaded");
    let mut base: DaemonConfig = detection_config(&dir, approver_base_config());
    base.plan_approval_required = true;
    let config: DaemonConfig = live_playbook_config(
        &dir,
        base,
        "[approval]\nauto_approve = [\"send_message\", \"close_window\"]\n\n[plans]\n\
         require_approval = false\n",
    );
    let h = Harness::start("settings-read-loaded", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let reply: DaemonMessage = call_get_approval_settings(&mut approver).await;
    let settings: ApprovalSettingsInfo = expect_approval_settings(reply);
    assert_eq!(settings.playbook, PlaybookState::Loaded);
    assert_eq!(tool_names(&settings.tools), expected_tool_order());
    assert_eq!(reported_auto_approve(&settings, "send_message"), Some(true));
    assert_eq!(reported_auto_approve(&settings, "close_window"), Some(true));
    assert_eq!(reported_auto_approve(&settings, "send_keys"), Some(false));
    assert!(!settings.plans_require_approval);

    // NotConfigured.
    let config2 = DaemonConfig {
        playbook: None,
        approval_policy: None,
        plan_approval_required: true,
        ..approver_base_config()
    };
    let h2 = Harness::start("settings-read-notconfigured", config2).await;
    let mut approver2 = h2.connect().await;
    register_approver(&mut approver2).await;
    let reply2: DaemonMessage = call_get_approval_settings(&mut approver2).await;
    let settings2: ApprovalSettingsInfo = expect_approval_settings(reply2);
    assert_eq!(settings2.playbook, PlaybookState::NotConfigured);
    assert_eq!(tool_names(&settings2.tools), expected_tool_order());
    assert!(
        settings2
            .tools
            .iter()
            .all(|entry: &ToolAutoApprove| !entry.auto_approve)
    );
    assert!(settings2.plans_require_approval);

    // Failed.
    let dir3 = unique_temp_dir("settings-read-failed");
    let (config3, _path3) = failed_at_boot_config(&dir3, approver_base_config());
    let h3 = Harness::start("settings-read-failed", config3).await;
    let mut approver3 = h3.connect().await;
    register_approver(&mut approver3).await;
    let reply3: DaemonMessage = call_get_approval_settings(&mut approver3).await;
    let settings3: ApprovalSettingsInfo = expect_approval_settings(reply3);
    assert_eq!(settings3.playbook, PlaybookState::Failed);
    assert_eq!(tool_names(&settings3.tools), expected_tool_order());
    assert!(
        settings3
            .tools
            .iter()
            .all(|entry: &ToolAutoApprove| !entry.auto_approve)
    );
    assert!(
        !settings3.plans_require_approval,
        "the plan-review flag must be read from the daemon's own config, not hardcoded"
    );
}

// =============================================================================
// A tool added to the file by hand is never picked up by a switch of a
// different tool: the daemon applies only the one differing entry it was
// asked to change.
// =============================================================================

#[tokio::test]
async fn a_tool_added_to_the_file_by_hand_stays_pending_while_a_different_tool_is_switched_on() {
    let dir = unique_temp_dir("settings-manual-file-edit");
    let config: DaemonConfig = live_playbook_config(
        &dir,
        detection_config(&dir, approver_base_config()),
        NO_AUTO_APPROVE_PLAYBOOK,
    );
    let path: PathBuf = config.playbook.clone().expect("playbook path must be set");
    let h = Harness::start("settings-manual-file-edit", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    // A human (or an agent) edits the file directly, adding send_keys.
    fs::write(&path, "[approval]\nauto_approve = [\"send_keys\"]\n").expect("manual edit");

    // The approver switches a different tool on through the daemon.
    let reply: DaemonMessage = call_set_auto_approve(&mut approver, "close_window", true).await;
    let settings: ApprovalSettingsInfo = expect_auto_approve_set(reply);
    assert_eq!(reported_auto_approve(&settings, "close_window"), Some(true));
    assert_eq!(
        reported_auto_approve(&settings, "send_keys"),
        Some(false),
        "the daemon's value for a tool it was never asked about must not silently follow a \
         manual file edit"
    );

    let contents: String = fs::read_to_string(&path).expect("read the rewritten file");
    assert!(
        contents.contains("send_keys"),
        "the file must still carry the manual entry"
    );
    assert!(
        contents.contains("close_window"),
        "the file must also carry the new entry"
    );

    let (mut requester, _own_window) = identified_window(&h).await;
    let window_id: String = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::SendKeys {
            window_id: window_id.clone(),
            keys: vec![ControlKey::Enter],
        },
    )
    .await;
    let request: ApprovalRequest = match read_approver_reply(&mut approver, REPLY_TIMEOUT).await {
        DaemonMessage::ApprovalRequested { request } => request,
        other => panic!("expected approval_requested, got {other:?}"),
    };
    assert_eq!(request.tool, "send_keys");
    deny_and_confirm(&mut approver, &mut requester, &request).await;

    let second_window: String = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id: second_window.clone(),
            force: None,
        },
    )
    .await;
    let reply: DaemonMessage = read_response(&mut requester, REPLY_TIMEOUT).await;
    match reply {
        DaemonMessage::WindowClosed { window_id } => assert_eq!(window_id, second_window),
        other => panic!("expected window_closed without approval, got {other:?}"),
    }

    h.registry.terminate_all().await;
}

// =============================================================================
// The file and the daemon disagree on one setting: the setting can still be
// switched in either direction, only the daemon's side changes, and the
// switch is logged.
// =============================================================================

#[tokio::test]
#[traced_test]
async fn switching_a_tool_to_match_a_manually_cleared_files_off_value_changes_only_the_daemon_and_logs_it()
 {
    let dir = unique_temp_dir("settings-mismatch-tool-off");
    let config: DaemonConfig = live_playbook_config(
        &dir,
        detection_config(&dir, approver_base_config()),
        "[approval]\nauto_approve = [\"send_keys\"]\n",
    );
    let path: PathBuf = config.playbook.clone().expect("playbook path must be set");
    let h = Harness::start("settings-mismatch-tool-off", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    // Manual edit: the file's own value is already off, but the daemon still
    // knows the tool as on, from boot.
    fs::write(&path, "[approval]\nauto_approve = []\n").expect("manual edit");
    let before: Vec<u8> = fs::read(&path).expect("read before");
    let mtime_before: SystemTime = fs::metadata(&path)
        .expect("stat before")
        .modified()
        .expect("mtime before");

    let reply: DaemonMessage = call_set_auto_approve(&mut approver, "send_keys", false).await;
    let settings: ApprovalSettingsInfo = expect_auto_approve_set(reply);
    assert_eq!(reported_auto_approve(&settings, "send_keys"), Some(false));

    let after: Vec<u8> = fs::read(&path).expect("read after");
    let mtime_after: SystemTime = fs::metadata(&path)
        .expect("stat after")
        .modified()
        .expect("mtime after");
    assert_eq!(
        before, after,
        "the file already matched the request, so it must not be rewritten"
    );
    assert_eq!(mtime_before, mtime_after);

    let (mut requester, _own_window) = identified_window(&h).await;
    let window_id: String = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::SendKeys {
            window_id,
            keys: vec![ControlKey::Enter],
        },
    )
    .await;
    let request: ApprovalRequest = match read_approver_reply(&mut approver, REPLY_TIMEOUT).await {
        DaemonMessage::ApprovalRequested { request } => request,
        other => panic!("expected approval_requested, got {other:?}"),
    };
    assert_eq!(request.tool, "send_keys");
    deny_and_confirm(&mut approver, &mut requester, &request).await;

    logs_assert(|lines: &[&str]| {
        let hits = lines
            .iter()
            .filter(|line| line.contains("send_keys") && line.contains("false"))
            .count();
        if hits == 1 {
            Ok(())
        } else {
            Err(format!(
                "expected exactly one info line naming send_keys and false, found {hits} in \
                 {lines:?}"
            ))
        }
    });

    h.registry.terminate_all().await;
}

#[tokio::test]
#[traced_test]
async fn switching_a_tool_to_match_a_manually_added_files_on_value_changes_only_the_daemon_and_logs_it()
 {
    let dir = unique_temp_dir("settings-mismatch-tool-on");
    let config: DaemonConfig = live_playbook_config(
        &dir,
        detection_config(&dir, approver_base_config()),
        NO_AUTO_APPROVE_PLAYBOOK,
    );
    let path: PathBuf = config.playbook.clone().expect("playbook path must be set");
    let h = Harness::start("settings-mismatch-tool-on", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    // Manual edit: the file's own value is already on, but the daemon still
    // knows the tool as off, from boot.
    fs::write(&path, "[approval]\nauto_approve = [\"send_keys\"]\n").expect("manual edit");
    let before: Vec<u8> = fs::read(&path).expect("read before");
    let mtime_before: SystemTime = fs::metadata(&path)
        .expect("stat before")
        .modified()
        .expect("mtime before");

    let reply: DaemonMessage = call_set_auto_approve(&mut approver, "send_keys", true).await;
    let settings: ApprovalSettingsInfo = expect_auto_approve_set(reply);
    assert_eq!(reported_auto_approve(&settings, "send_keys"), Some(true));

    let after: Vec<u8> = fs::read(&path).expect("read after");
    let mtime_after: SystemTime = fs::metadata(&path)
        .expect("stat after")
        .modified()
        .expect("mtime after");
    assert_eq!(
        before, after,
        "the file already matched the request, so it must not be rewritten"
    );
    assert_eq!(mtime_before, mtime_after);

    let (mut requester, _own_window) = identified_window(&h).await;
    let window_id: String = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::SendKeys {
            window_id: window_id.clone(),
            keys: vec![ControlKey::Enter],
        },
    )
    .await;
    let reply: DaemonMessage = read_response(&mut requester, REPLY_TIMEOUT).await;
    match reply {
        DaemonMessage::KeysSent { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected keys_sent without approval, got {other:?}"),
    }

    logs_assert(|lines: &[&str]| {
        let hits = lines
            .iter()
            .filter(|line| line.contains("send_keys") && line.contains("true"))
            .count();
        if hits == 1 {
            Ok(())
        } else {
            Err(format!(
                "expected exactly one info line naming send_keys and true, found {hits} in \
                 {lines:?}"
            ))
        }
    });

    h.registry.terminate_all().await;
}

// --- plan review: the same two directions -------------------------------------

const PLAN_TASK_PROFILE: &str = "settings-plan-task";

fn plan_task(task_id: &str) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: task_id.to_string(),
        title: format!("task {task_id}"),
        role: None,
        profile: Some(PLAN_TASK_PROFILE.to_string()),
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: None,
    }
}

fn expect_plan_detail(reply: DaemonMessage) -> PlanInfo {
    match reply {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

async fn create_plan(owner: &mut Connection, task_id: &str) -> PlanInfo {
    send(
        owner,
        &ClientMessage::CreatePlan {
            title: format!("plan with {task_id}"),
            tasks: vec![plan_task(task_id)],
        },
    )
    .await;
    let reply: DaemonMessage = read_approver_reply(owner, REPLY_TIMEOUT).await;
    expect_plan_detail(reply)
}

fn plan_review_config(dir: &Path, require_approval_at_boot: bool) -> (DaemonConfig, PathBuf) {
    let profiles_dir = dir.join("profiles");
    fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_profile(
        &profiles_dir,
        PLAN_TASK_PROFILE,
        &ready_child_profile_source("Settings plan task"),
    );
    let mut base: DaemonConfig = detection_config(dir, approver_base_config());
    base.store_db_path_resolved = Some(dir.join("plan.db"));
    base.profiles_dir = Some(profiles_dir);
    let source = if require_approval_at_boot {
        "[approval]\nauto_approve = []\n\n[plans]\nrequire_approval = true\n"
    } else {
        "[approval]\nauto_approve = []\n\n[plans]\nrequire_approval = false\n"
    };
    let config: DaemonConfig = live_playbook_config(dir, base, source);
    let path: PathBuf = config.playbook.clone().expect("playbook path must be set");
    (config, path)
}

#[tokio::test]
#[traced_test]
async fn switching_plan_review_off_to_match_a_manually_cleared_files_value_changes_only_the_daemon_and_logs_it()
 {
    let dir = unique_temp_dir("settings-plan-mismatch-off");
    let (config, path) = plan_review_config(&dir, true);
    let (h, _locks): (Harness, std::sync::Arc<LockTable>) =
        Harness::start_with_ledger("settings-plan-mismatch-off", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    fs::write(
        &path,
        "[approval]\nauto_approve = []\n\n[plans]\nrequire_approval = false\n",
    )
    .expect("manual edit");
    let before: Vec<u8> = fs::read(&path).expect("read before");
    let mtime_before: SystemTime = fs::metadata(&path)
        .expect("stat before")
        .modified()
        .expect("mtime before");

    let reply: DaemonMessage = call_set_plan_approval_required(&mut approver, false).await;
    let settings: ApprovalSettingsInfo = expect_plan_approval_required_set(reply);
    assert!(!settings.plans_require_approval);

    let after: Vec<u8> = fs::read(&path).expect("read after");
    let mtime_after: SystemTime = fs::metadata(&path)
        .expect("stat after")
        .modified()
        .expect("mtime after");
    assert_eq!(
        before, after,
        "the file already matched the request, so it must not be rewritten"
    );
    assert_eq!(mtime_before, mtime_after);

    let (mut owner, _owner_id) = identified_window(&h).await;
    let plan: PlanInfo = create_plan(&mut owner, "t1").await;
    assert_eq!(
        plan.review, None,
        "a plan created after review was switched off must not wait for review"
    );

    logs_assert(|lines: &[&str]| {
        let hits = lines
            .iter()
            .filter(|line| line.contains("plan_approval_required") && line.contains("false"))
            .count();
        if hits == 1 {
            Ok(())
        } else {
            Err(format!(
                "expected exactly one info line naming plan_approval_required and false, found \
                 {hits} in {lines:?}"
            ))
        }
    });

    h.registry.terminate_all().await;
}

#[tokio::test]
#[traced_test]
async fn switching_plan_review_on_to_match_a_manually_set_files_value_changes_only_the_daemon_and_logs_it()
 {
    let dir = unique_temp_dir("settings-plan-mismatch-on");
    let (config, path) = plan_review_config(&dir, false);
    let (h, _locks): (Harness, std::sync::Arc<LockTable>) =
        Harness::start_with_ledger("settings-plan-mismatch-on", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    fs::write(
        &path,
        "[approval]\nauto_approve = []\n\n[plans]\nrequire_approval = true\n",
    )
    .expect("manual edit");
    let before: Vec<u8> = fs::read(&path).expect("read before");
    let mtime_before: SystemTime = fs::metadata(&path)
        .expect("stat before")
        .modified()
        .expect("mtime before");

    let reply: DaemonMessage = call_set_plan_approval_required(&mut approver, true).await;
    let settings: ApprovalSettingsInfo = expect_plan_approval_required_set(reply);
    assert!(settings.plans_require_approval);

    let after: Vec<u8> = fs::read(&path).expect("read after");
    let mtime_after: SystemTime = fs::metadata(&path)
        .expect("stat after")
        .modified()
        .expect("mtime after");
    assert_eq!(
        before, after,
        "the file already matched the request, so it must not be rewritten"
    );
    assert_eq!(mtime_before, mtime_after);

    let (mut owner, _owner_id) = identified_window(&h).await;
    let plan: PlanInfo = create_plan(&mut owner, "t1").await;
    assert_eq!(
        plan.review,
        Some(yagura_protocol::PlanReview::Pending),
        "a plan created after review was switched back on must wait for review"
    );

    logs_assert(|lines: &[&str]| {
        let hits = lines
            .iter()
            .filter(|line| line.contains("plan_approval_required") && line.contains("true"))
            .count();
        if hits == 1 {
            Ok(())
        } else {
            Err(format!(
                "expected exactly one info line naming plan_approval_required and true, found \
                 {hits} in {lines:?}"
            ))
        }
    });

    h.registry.terminate_all().await;
}

// =============================================================================
// Audit log: exactly one line per change, none for a request that already
// matches, warn (not info) for a failed write.
// =============================================================================

#[tokio::test]
#[traced_test]
async fn switching_a_tool_on_logs_once_and_a_repeat_of_the_same_value_logs_nothing_further() {
    let dir = unique_temp_dir("settings-log-auto");
    let config: DaemonConfig = live_playbook_config(
        &dir,
        detection_config(&dir, approver_base_config()),
        NO_AUTO_APPROVE_PLAYBOOK,
    );
    let path: PathBuf = config.playbook.clone().expect("playbook path must be set");
    let h = Harness::start("settings-log-auto", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    // Positive control: the first switch actually changes both the file and
    // the daemon's value, and is logged.
    let reply: DaemonMessage = call_set_auto_approve(&mut approver, "send_keys", true).await;
    let settings: ApprovalSettingsInfo = expect_auto_approve_set(reply);
    assert_eq!(reported_auto_approve(&settings, "send_keys"), Some(true));

    let before: Vec<u8> = fs::read(&path).expect("read before the no-op");

    // A repeat of the same value changes neither the file nor the daemon.
    let reply: DaemonMessage = call_set_auto_approve(&mut approver, "send_keys", true).await;
    let settings: ApprovalSettingsInfo = expect_auto_approve_set(reply);
    assert_eq!(reported_auto_approve(&settings, "send_keys"), Some(true));

    tokio::time::sleep(NEGATIVE_LOG_OBSERVATION).await;

    let after: Vec<u8> = fs::read(&path).expect("read after the no-op");
    assert_eq!(
        before, after,
        "a request that already matches must not rewrite the file"
    );

    logs_assert(|lines: &[&str]| {
        let hits = lines
            .iter()
            .filter(|line| line.contains("send_keys") && line.contains("true"))
            .count();
        if hits == 1 {
            Ok(())
        } else {
            Err(format!(
                "a no-op switch must add no further matching log line beyond the first switch's \
                 own, found {hits} in {lines:?}"
            ))
        }
    });

    h.registry.terminate_all().await;
}

#[tokio::test]
#[traced_test]
async fn switching_plan_review_off_logs_once_and_a_repeat_of_the_same_value_logs_nothing_further() {
    let dir = unique_temp_dir("settings-log-plan");
    let config: DaemonConfig = live_playbook_config(
        &dir,
        detection_config(&dir, approver_base_config()),
        "[approval]\nauto_approve = []\n\n[plans]\nrequire_approval = true\n",
    );
    let path: PathBuf = config.playbook.clone().expect("playbook path must be set");
    let h = Harness::start("settings-log-plan", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let reply: DaemonMessage = call_set_plan_approval_required(&mut approver, false).await;
    let settings: ApprovalSettingsInfo = expect_plan_approval_required_set(reply);
    assert!(!settings.plans_require_approval);

    let before: Vec<u8> = fs::read(&path).expect("read before the no-op");

    let reply: DaemonMessage = call_set_plan_approval_required(&mut approver, false).await;
    let settings: ApprovalSettingsInfo = expect_plan_approval_required_set(reply);
    assert!(!settings.plans_require_approval);

    tokio::time::sleep(NEGATIVE_LOG_OBSERVATION).await;

    let after: Vec<u8> = fs::read(&path).expect("read after the no-op");
    assert_eq!(
        before, after,
        "a request that already matches must not rewrite the file"
    );

    logs_assert(|lines: &[&str]| {
        let hits = lines
            .iter()
            .filter(|line| line.contains("plan_approval_required") && line.contains("false"))
            .count();
        if hits == 1 {
            Ok(())
        } else {
            Err(format!(
                "a no-op switch must add no further matching log line beyond the first switch's \
                 own, found {hits} in {lines:?}"
            ))
        }
    });

    h.registry.terminate_all().await;
}

#[tokio::test]
#[traced_test]
async fn a_failed_write_is_logged_at_warn_not_info() {
    let dir = unique_temp_dir("settings-log-warn");
    let config: DaemonConfig = live_playbook_config(
        &dir,
        detection_config(&dir, approver_base_config()),
        NO_AUTO_APPROVE_PLAYBOOK,
    );
    let path: PathBuf = config.playbook.clone().expect("playbook path must be set");
    let h = Harness::start("settings-log-warn", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    block_rewrite(&path, true);
    let reply: DaemonMessage = call_set_auto_approve(&mut approver, "send_keys", true).await;
    expect_error(reply, "internal");

    logs_assert(|lines: &[&str]| {
        let hit = lines
            .iter()
            .find(|line| line.contains("failed to replace playbook file"));
        match hit {
            Some(line) if line.contains("WARN") => Ok(()),
            Some(line) => Err(format!(
                "expected the failure line at warn level, got: {line}"
            )),
            None => Err("expected a line reporting the failed write".to_string()),
        }
    });

    block_rewrite(&path, false);
    h.registry.terminate_all().await;
}
