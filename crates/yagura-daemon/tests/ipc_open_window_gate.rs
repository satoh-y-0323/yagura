#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: starting a window is a gated operation (`open_window`).
//!
//! The gate covers every way an agent can start a process: `open_window` with
//! a command, `open_window` with a profile, a profile-addressed
//! `send_handoff` and `open_task_window`. These cases drive each of them over
//! a real connection and measure what the requester and an approver see, and
//! whether a window exists at each point.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{
    Harness, approver_base_config, call_list_windows, decide, empty_lookup,
    expect_approval_requested, expect_approval_requested_within, expect_error,
    handoff_profile_config, identify, policy, read_approver_reply, read_response,
    register_approver, send, send_handoff_from_profile, write_profile,
};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{
    ApprovalRequest, ApprovalRequesterKind, ClientMessage, DaemonMessage, PlanTaskSpec,
    WithdrawReason,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::{
    echo_child_command, ready_child_profile_source, ready_child_profile_source_with_composer,
};

const PROFILE_ID: &str = "gate-profile";

fn config_with(auto_approve: &[&str], approval_timeout_secs: u64) -> DaemonConfig {
    let mut config = approver_base_config();
    config.approval_policy = Some(policy(auto_approve));
    config.approval_timeout_secs = approval_timeout_secs;
    config
}

/// A config that can open `PROFILE_ID` (a plain `ready_child` window) from
/// `dir`, with the plan ledger's database beside it.
fn profile_config(dir: &Path, auto_approve: &[&str]) -> DaemonConfig {
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_profile(
        &profiles_dir,
        PROFILE_ID,
        &ready_child_profile_source("Gate Profile"),
    );
    let mut config = config_with(auto_approve, 30);
    config.profiles_dir = Some(profiles_dir);
    config.store_db_path_resolved = Some(dir.join("plan.db"));
    config.plan_approval_required = false;
    config
}

/// A handoff config whose policy auto-approves exactly `auto_approve`:
/// `handoff_profile_config` adds `open_window` to what it is given, which is
/// what this file must not have.
fn gated_handoff_config(
    profiles_dir: std::path::PathBuf,
    detection_dir: &Path,
    auto_approve: &[&str],
) -> DaemonConfig {
    let mut config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        detection_dir,
        None,
        None,
        30,
    );
    config.approval_policy = Some(policy(auto_approve));
    config
}

async fn approver_connection(h: &Harness) -> Connection {
    let mut client = h.connect().await;
    register_approver(&mut client).await;
    client
}

/// A connection identified as a freshly opened window, which is how an agent
/// in a yagura window reaches the daemon.
async fn window_connection(h: &Harness) -> Connection {
    let window_id = h.open(echo_child_command(), None).await;
    let mut client = h.connect().await;
    identify(&mut client, &window_id).await;
    client
}

async fn window_count(h: &Harness) -> usize {
    let mut observer = h.connect().await;
    call_list_windows(&mut observer).await.len()
}

async fn send_open_command(client: &mut Connection, command: Vec<String>, cwd: Option<&str>) {
    send(
        client,
        &ClientMessage::OpenWindow {
            command,
            cwd: cwd.map(str::to_string),
            name: None,
            vendor: None,
            detection: None,
        },
    )
    .await;
}

async fn send_open_profile(client: &mut Connection) {
    send(
        client,
        &ClientMessage::OpenWindowFromProfile {
            profile: PROFILE_ID.to_string(),
            cwd: None,
            name: None,
            model: None,
            effort: None,
        },
    )
    .await;
}

fn expect_window_opened(msg: DaemonMessage) -> String {
    match msg {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected window_opened, got {other:?}"),
    }
}

/// The lines an approver is shown for a request to open a window.
fn text_lines(request: &ApprovalRequest) -> Vec<&str> {
    request
        .text
        .as_deref()
        .expect("an open_window request must carry a description")
        .lines()
        .collect()
}

fn assert_has_no_window_to_act_on(request: &ApprovalRequest) {
    assert_eq!(request.tool, "open_window");
    assert!(
        request.target.is_none(),
        "the window does not exist yet: {request:?}"
    );
    assert!(request.room.is_none(), "{request:?}");
}

// --- open_window with a command --------------------------------------------

#[tokio::test]
async fn a_windows_open_window_waits_for_an_approver_and_opens_once_approved() {
    let h = Harness::start("open-gate-command-approve", config_with(&[], 30)).await;
    let mut approver = approver_connection(&h).await;
    let mut requester = window_connection(&h).await;
    let before = window_count(&h).await;

    let dir = TempDir::new("yagura-cli-test", "open-gate-command-cwd");
    let cwd = dir.make_dir("work");
    let mut command = echo_child_command();
    command.push("two words".to_string());
    send_open_command(&mut requester, command.clone(), cwd.to_str()).await;

    let request = expect_approval_requested(&mut approver).await;
    assert_has_no_window_to_act_on(&request);
    assert_eq!(request.requester.kind, ApprovalRequesterKind::Window);
    let lines = text_lines(&request);
    assert_eq!(lines.len(), 2, "command and cwd lines: {lines:?}");
    assert!(
        lines[0].starts_with("command: ") && lines[0].ends_with(" \"two words\""),
        "the argument holding a space is quoted: {lines:?}"
    );
    let shown_cwd = lines[1]
        .strip_prefix("cwd: ")
        .unwrap_or_else(|| panic!("second line must be the cwd: {lines:?}"));
    assert!(
        Path::new(shown_cwd).ends_with("work"),
        "the cwd shown must be the requested directory: {shown_cwd}"
    );
    assert_eq!(
        window_count(&h).await,
        before,
        "no window may exist while the request waits"
    );

    decide(&mut approver, &request.request_id, true).await;
    let window_id =
        expect_window_opened(read_response(&mut requester, Duration::from_secs(10)).await);
    assert!(h.registry.get(&window_id).is_some());
    assert_eq!(window_count(&h).await, before + 1);
}

#[tokio::test]
async fn a_denied_open_window_opens_nothing() {
    let h = Harness::start("open-gate-command-deny", config_with(&[], 30)).await;
    let mut approver = approver_connection(&h).await;
    let mut requester = window_connection(&h).await;
    let before = window_count(&h).await;

    send_open_command(&mut requester, echo_child_command(), None).await;
    let request = expect_approval_requested(&mut approver).await;
    decide(&mut approver, &request.request_id, false).await;

    expect_error(
        read_response(&mut requester, Duration::from_secs(5)).await,
        "approval_denied",
    );
    assert_eq!(window_count(&h).await, before);
}

#[tokio::test]
async fn an_open_window_without_a_decision_times_out_and_opens_nothing() {
    let h = Harness::start("open-gate-command-timeout", config_with(&[], 1)).await;
    let mut approver = approver_connection(&h).await;
    let mut requester = window_connection(&h).await;
    let before = window_count(&h).await;

    send_open_command(&mut requester, echo_child_command(), None).await;
    expect_approval_requested(&mut approver).await;

    expect_error(
        read_response(&mut requester, Duration::from_secs(10)).await,
        "approval_timeout",
    );
    assert_eq!(window_count(&h).await, before);
}

#[tokio::test]
async fn an_external_open_window_with_no_approver_is_refused_as_approval_required() {
    let h = Harness::start("open-gate-no-approver", config_with(&[], 30)).await;
    let mut external = h.connect().await;
    let before = window_count(&h).await;

    send_open_command(&mut external, echo_child_command(), None).await;

    expect_error(
        read_response(&mut external, Duration::from_secs(5)).await,
        "approval_required",
    );
    assert_eq!(window_count(&h).await, before);
}

#[tokio::test]
async fn open_window_listed_in_auto_approve_opens_without_asking_anyone() {
    let h = Harness::start("open-gate-auto", config_with(&["open_window"], 30)).await;
    let mut requester = window_connection(&h).await;

    send_open_command(&mut requester, echo_child_command(), None).await;

    // No approver is connected, so an approval request would be refused.
    expect_window_opened(read_response(&mut requester, Duration::from_secs(10)).await);
}

#[tokio::test]
async fn an_approver_opens_a_window_without_waiting_for_anyone() {
    let h = Harness::start("open-gate-approver", config_with(&[], 30)).await;
    let mut approver = approver_connection(&h).await;

    send_open_command(&mut approver, echo_child_command(), None).await;

    expect_window_opened(read_approver_reply(&mut approver, Duration::from_secs(10)).await);
}

#[tokio::test]
async fn a_request_too_long_to_show_an_approver_is_refused_instead_of_shown_cut_off() {
    let h = Harness::start("open-gate-too-long", config_with(&[], 30)).await;
    let _approver = approver_connection(&h).await;
    let mut requester = window_connection(&h).await;
    let before = window_count(&h).await;

    let mut command = echo_child_command();
    command.push("a".repeat(yagura_protocol::APPROVAL_TEXT_MAX_BYTES + 1));
    send_open_command(&mut requester, command, None).await;

    expect_error(
        read_response(&mut requester, Duration::from_secs(5)).await,
        "invalid_argument",
    );
    assert_eq!(window_count(&h).await, before);
}

#[tokio::test]
async fn a_broken_playbook_refuses_an_agents_open_window() {
    let mut config = config_with(&[], 30);
    config.approval_policy = None;
    config.playbook = Some(common::guaranteed_nonexistent_exe_path());
    let h = Harness::start("open-gate-broken-playbook", config).await;
    let mut requester = window_connection(&h).await;

    send_open_command(&mut requester, echo_child_command(), None).await;

    let message = expect_error(
        read_response(&mut requester, Duration::from_secs(5)).await,
        "approval_required",
    );
    assert!(message.contains("playbook failed to load"), "{message}");
}

// --- open_window with a profile ---------------------------------------------

#[tokio::test]
async fn a_profile_open_window_asks_about_the_profile_and_opens_once_approved() {
    let dir = TempDir::new("yagura-cli-test", "open-gate-profile");
    let h = Harness::start("open-gate-profile", profile_config(dir.path(), &[])).await;
    let mut approver = approver_connection(&h).await;
    let mut requester = window_connection(&h).await;
    let before = window_count(&h).await;

    send_open_profile(&mut requester).await;

    let request = expect_approval_requested(&mut approver).await;
    assert_has_no_window_to_act_on(&request);
    let lines = text_lines(&request);
    assert_eq!(lines[0], format!("profile: {PROFILE_ID}"));
    assert!(lines[1].starts_with("cwd: "), "{lines:?}");
    assert_eq!(lines.len(), 2, "a plain profile open names no purpose");
    assert_eq!(window_count(&h).await, before);

    decide(&mut approver, &request.request_id, true).await;
    expect_window_opened(read_response(&mut requester, Duration::from_secs(10)).await);
    assert_eq!(window_count(&h).await, before + 1);
}

#[tokio::test]
async fn a_denied_profile_open_window_opens_nothing() {
    let dir = TempDir::new("yagura-cli-test", "open-gate-profile-deny");
    let h = Harness::start("open-gate-profile-deny", profile_config(dir.path(), &[])).await;
    let mut approver = approver_connection(&h).await;
    let mut requester = window_connection(&h).await;
    let before = window_count(&h).await;

    send_open_profile(&mut requester).await;
    let request = expect_approval_requested(&mut approver).await;
    decide(&mut approver, &request.request_id, false).await;

    expect_error(
        read_response(&mut requester, Duration::from_secs(5)).await,
        "approval_denied",
    );
    assert_eq!(window_count(&h).await, before);
}

// --- send_handoff from a profile --------------------------------------------

#[tokio::test]
async fn a_profile_handoff_asks_to_start_the_window_before_it_exists_and_to_hand_over_after() {
    let tree = TempDir::new("yagura-cli-test", "open-gate-handoff");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_composer("Ready", 0),
    );
    let h = Harness::start(
        "open-gate-handoff",
        gated_handoff_config(profiles_dir, &detection_dir, &[]),
    )
    .await;
    let mut approver = approver_connection(&h).await;
    let mut requester = window_connection(&h).await;
    let before = window_count(&h).await;

    send_handoff_from_profile(&mut requester, "ready", None, None, "src/foo.rs", "look").await;

    let open_request =
        expect_approval_requested_within(&mut approver, Duration::from_secs(10)).await;
    assert_has_no_window_to_act_on(&open_request);
    let lines = text_lines(&open_request);
    assert_eq!(lines[0], "profile: ready");
    assert_eq!(lines.last().copied(), Some("for: handoff"), "{lines:?}");
    assert_eq!(
        window_count(&h).await,
        before,
        "the process must not be started before its start is approved"
    );

    decide(&mut approver, &open_request.request_id, true).await;

    let handoff_request =
        expect_approval_requested_within(&mut approver, Duration::from_secs(15)).await;
    assert_eq!(handoff_request.tool, "send_handoff");
    assert_eq!(window_count(&h).await, before + 1);

    decide(&mut approver, &handoff_request.request_id, false).await;
    expect_error(
        read_response(&mut requester, Duration::from_secs(10)).await,
        "approval_denied",
    );
}

#[tokio::test]
async fn a_profile_handoff_whose_start_is_denied_opens_no_window_and_gives_its_budget_back() {
    let tree = TempDir::new("yagura-cli-test", "open-gate-handoff-deny");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_composer("Ready", 0),
    );
    let h = Harness::start(
        "open-gate-handoff-deny",
        gated_handoff_config(profiles_dir, &detection_dir, &["send_handoff"]),
    )
    .await;
    let mut approver = approver_connection(&h).await;
    let mut requester = window_connection(&h).await;
    let before = window_count(&h).await;

    send_handoff_from_profile(&mut requester, "ready", None, None, "src/foo.rs", "look").await;
    let open_request =
        expect_approval_requested_within(&mut approver, Duration::from_secs(10)).await;
    assert_eq!(open_request.tool, "open_window");
    decide(&mut approver, &open_request.request_id, false).await;

    expect_error(
        read_response(&mut requester, Duration::from_secs(5)).await,
        "approval_denied",
    );
    assert_eq!(window_count(&h).await, before);
}

// --- open_task_window -------------------------------------------------------

fn task(task_id: &str) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: task_id.to_string(),
        title: "do the thing".to_string(),
        role: None,
        profile: Some(PROFILE_ID.to_string()),
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: None,
    }
}

async fn read_plan_reply(client: &mut Connection, timeout: Duration) -> DaemonMessage {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for a reply");
        let frame = read_response(client, remaining).await;
        if !matches!(frame, DaemonMessage::PlanChanged { .. }) {
            return frame;
        }
    }
}

async fn create_one_task_plan(owner: &mut Connection) -> String {
    send(
        owner,
        &ClientMessage::CreatePlan {
            title: "plan".to_string(),
            tasks: vec![task("t1")],
        },
    )
    .await;
    match read_plan_reply(owner, Duration::from_secs(5)).await {
        DaemonMessage::PlanDetail { plan } => plan.plan_id,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

/// The next approval request on an approver connection, skipping the
/// `plan_changed` pushes an approver is sent while plans change.
async fn expect_open_request(approver: &mut Connection) -> ApprovalRequest {
    match read_approver_reply(approver, Duration::from_secs(5)).await {
        DaemonMessage::ApprovalRequested { request } => request,
        other => panic!("expected approval_requested, got {other:?}"),
    }
}

async fn send_open_task_window(owner: &mut Connection, plan_id: &str) {
    send(
        owner,
        &ClientMessage::OpenTaskWindow {
            plan_id: plan_id.to_string(),
            task_id: "t1".to_string(),
            name: None,
        },
    )
    .await;
}

#[tokio::test]
async fn open_task_window_asks_before_the_task_window_exists_and_binds_it_once_approved() {
    let dir = TempDir::new("yagura-cli-test", "open-gate-task");
    let (h, _locks) =
        Harness::start_with_ledger("open-gate-task", profile_config(dir.path(), &[])).await;
    let mut approver = approver_connection(&h).await;
    let mut owner = window_connection(&h).await;
    let plan_id = create_one_task_plan(&mut owner).await;
    let before = window_count(&h).await;

    send_open_task_window(&mut owner, &plan_id).await;

    let request = expect_open_request(&mut approver).await;
    assert_has_no_window_to_act_on(&request);
    let lines = text_lines(&request);
    assert_eq!(lines[0], format!("profile: {PROFILE_ID}"));
    assert_eq!(
        lines.last().copied(),
        Some("for: plan task \"t1\""),
        "{lines:?}"
    );
    assert_eq!(window_count(&h).await, before);

    decide(&mut approver, &request.request_id, true).await;
    match read_plan_reply(&mut owner, Duration::from_secs(10)).await {
        DaemonMessage::TaskWindowOpened { task_id, .. } => assert_eq!(task_id, "t1"),
        other => panic!("expected task_window_opened, got {other:?}"),
    }
    assert_eq!(window_count(&h).await, before + 1);
}

#[tokio::test]
async fn a_denied_task_window_leaves_the_task_unbound_and_the_plan_usable() {
    let dir = TempDir::new("yagura-cli-test", "open-gate-task-deny");
    let (h, _locks) =
        Harness::start_with_ledger("open-gate-task-deny", profile_config(dir.path(), &[])).await;
    let mut approver = approver_connection(&h).await;
    let mut owner = window_connection(&h).await;
    let plan_id = create_one_task_plan(&mut owner).await;
    let before = window_count(&h).await;

    send_open_task_window(&mut owner, &plan_id).await;
    let request = expect_open_request(&mut approver).await;
    decide(&mut approver, &request.request_id, false).await;
    expect_error(
        read_plan_reply(&mut owner, Duration::from_secs(5)).await,
        "approval_denied",
    );
    assert_eq!(window_count(&h).await, before);

    // The plan's lock was released with the refusal: the same task can be
    // asked for again.
    send_open_task_window(&mut owner, &plan_id).await;
    let again = expect_open_request(&mut approver).await;
    assert_has_no_window_to_act_on(&again);
}

#[tokio::test]
async fn open_task_window_listed_in_auto_approve_binds_without_asking() {
    let dir = TempDir::new("yagura-cli-test", "open-gate-task-auto");
    let (h, _locks) = Harness::start_with_ledger(
        "open-gate-task-auto",
        profile_config(dir.path(), &["open_window"]),
    )
    .await;
    let mut owner = window_connection(&h).await;
    let plan_id = create_one_task_plan(&mut owner).await;

    send_open_task_window(&mut owner, &plan_id).await;

    match read_plan_reply(&mut owner, Duration::from_secs(10)).await {
        DaemonMessage::TaskWindowOpened { .. } => {}
        other => panic!("expected task_window_opened, got {other:?}"),
    }
}

// --- what an approver reads ------------------------------------------------

#[tokio::test]
async fn the_command_an_approver_reads_keeps_argument_boundaries_and_escapes_control_characters() {
    let h = Harness::start("open-gate-command-quoting", config_with(&[], 30)).await;
    let mut approver = approver_connection(&h).await;
    let mut requester = window_connection(&h).await;

    let args = |args: &[&str]| {
        args.iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>()
    };
    send_open_command(&mut requester, args(&["sh", "-c", "rm -rf x"]), None).await;
    send_open_command(&mut requester, args(&["sh", "-c", "rm", "-rf", "x"]), None).await;
    send_open_command(
        &mut requester,
        args(&["tool", "", "a\"b", "back\\slash", "line\nbreak", "plain"]),
        None,
    )
    .await;

    let mut shown = Vec::new();
    for _ in 0..3 {
        let request = expect_approval_requested(&mut approver).await;
        shown.push(text_lines(&request)[0].to_string());
    }
    shown.sort();
    assert_eq!(
        shown,
        vec![
            r#"command: sh -c "rm -rf x""#.to_string(),
            "command: sh -c rm -rf x".to_string(),
            r#"command: tool "" "a\"b" "back\\slash" "line\nbreak" plain"#.to_string(),
        ]
    );
}

// --- refusing before asking ------------------------------------------------

#[tokio::test]
async fn an_open_window_that_cannot_open_is_refused_without_asking_an_approver() {
    let h = Harness::start("open-gate-command-validation", config_with(&[], 30)).await;
    let mut approver = approver_connection(&h).await;
    let mut requester = window_connection(&h).await;
    let dir = TempDir::new("yagura-cli-test", "open-gate-command-missing-cwd");
    let missing = dir.path().join("missing");

    send_open_command(&mut requester, Vec::new(), None).await;
    expect_error(
        read_response(&mut requester, Duration::from_secs(5)).await,
        "invalid_argument",
    );
    send_open_command(&mut requester, echo_child_command(), missing.to_str()).await;
    expect_error(
        read_response(&mut requester, Duration::from_secs(5)).await,
        "invalid_argument",
    );

    // The first request the approver sees is the valid one that follows, so
    // neither refusal queued an approval.
    send_open_command(&mut requester, vec!["valid-command".to_string()], None).await;
    let request = expect_approval_requested(&mut approver).await;
    assert_eq!(text_lines(&request)[0], "command: valid-command");
}

#[tokio::test]
async fn an_open_window_over_the_window_limit_is_refused_before_anyone_is_asked() {
    let mut config = config_with(&[], 30);
    config.max_windows = 1;
    let h = Harness::start("open-gate-window-limit", config).await;
    let mut requester = window_connection(&h).await;

    // No approver is connected: a request that reached the gate would be
    // refused as `approval_required`.
    send_open_command(&mut requester, echo_child_command(), None).await;
    expect_error(
        read_response(&mut requester, Duration::from_secs(5)).await,
        "resource_exhausted",
    );
}

// --- the requester leaves while the approval waits -------------------------

async fn expect_withdrawn_because_requester_left(approver: &mut Connection, request_id: &str) {
    match read_approver_reply(approver, Duration::from_secs(5)).await {
        DaemonMessage::ApprovalWithdrawn {
            request_id: withdrawn,
            reason,
        } => {
            assert_eq!(withdrawn, request_id);
            assert_eq!(reason, WithdrawReason::RequesterDisconnected);
        }
        other => panic!("expected approval_withdrawn, got {other:?}"),
    }
}

#[tokio::test]
async fn a_requester_leaving_during_an_open_window_approval_withdraws_it_and_opens_nothing() {
    let h = Harness::start("open-gate-command-leaves", config_with(&[], 30)).await;
    let mut approver = approver_connection(&h).await;
    let mut requester = window_connection(&h).await;
    let before = window_count(&h).await;

    send_open_command(&mut requester, echo_child_command(), None).await;
    let request = expect_approval_requested(&mut approver).await;
    drop(requester);

    expect_withdrawn_because_requester_left(&mut approver, &request.request_id).await;
    assert_eq!(window_count(&h).await, before);
}

#[tokio::test]
async fn a_profile_handoff_whose_requester_leaves_during_the_start_approval_gives_its_budget_back()
{
    let tree = TempDir::new("yagura-cli-test", "open-gate-handoff-leaves");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_composer("Ready", 0),
    );
    let mut config = gated_handoff_config(profiles_dir, &detection_dir, &[]);
    config.max_open_handoffs = 1;
    let h = Harness::start("open-gate-handoff-leaves", config).await;
    let mut approver = approver_connection(&h).await;
    let mut first = window_connection(&h).await;
    let mut second = window_connection(&h).await;
    let before = window_count(&h).await;

    send_handoff_from_profile(&mut first, "ready", None, None, "src/foo.rs", "look").await;
    let request = expect_approval_requested_within(&mut approver, Duration::from_secs(10)).await;
    drop(first);
    expect_withdrawn_because_requester_left(&mut approver, &request.request_id).await;

    // With the only handoff slot still held this would be answered
    // `resource_exhausted` instead of asking.
    send_handoff_from_profile(&mut second, "ready", None, None, "src/foo.rs", "look").await;
    let again = expect_approval_requested_within(&mut approver, Duration::from_secs(10)).await;
    assert_has_no_window_to_act_on(&again);
    assert_eq!(window_count(&h).await, before);
}

#[tokio::test]
async fn a_task_window_whose_requester_leaves_during_the_approval_frees_the_plan_to_ask_again() {
    let dir = TempDir::new("yagura-cli-test", "open-gate-task-leaves");
    let (h, _locks) =
        Harness::start_with_ledger("open-gate-task-leaves", profile_config(dir.path(), &[])).await;
    let mut approver = approver_connection(&h).await;
    let owner_window = h.open(echo_child_command(), None).await;
    let mut owner = h.connect().await;
    identify(&mut owner, &owner_window).await;
    let plan_id = create_one_task_plan(&mut owner).await;
    let before = window_count(&h).await;

    send_open_task_window(&mut owner, &plan_id).await;
    let request = expect_open_request(&mut approver).await;
    drop(owner);
    expect_withdrawn_because_requester_left(&mut approver, &request.request_id).await;

    // The plan's lock went with the withdrawn request: a held lock would be
    // answered with an error instead of asking again.
    let mut owner = h.connect().await;
    identify(&mut owner, &owner_window).await;
    send_open_task_window(&mut owner, &plan_id).await;
    let again = expect_open_request(&mut approver).await;
    assert_has_no_window_to_act_on(&again);
    assert_eq!(window_count(&h).await, before);
}
