#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the daemon's record of which windows have an identified
//! MCP bridge connection (`mcp_connected` on `list_windows` /
//! `get_window_status`) and the `awaiting` list an empty `wait_for_report`
//! answer carries. Driven through a real `Harness` over a real IPC endpoint.

mod common;

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use tokio::io::AsyncReadExt;

use common::{
    Harness, approver_base_config, call_list_windows, call_open_window, expect_error, identify,
    policy, read_response, register_approver, send, unique_temp_dir, wait_until_status,
    write_profile,
};

use yagura_daemon::DaemonConfig;
use yagura_protocol::{
    AwaitingWindow, ClientMessage, DaemonMessage, PlanInfo, PlanTaskSpec, ReportOutcome,
    WindowStatus,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{
    echo_child_command, exit_with_code_command, ready_child_profile_source,
};

/// Well-formed, but not the token the harness injects into its windows.
const WRONG_TOKEN: &str = "fedcba9876543210fedcba9876543210";

/// How long a connection's end may take to reach the daemon's record.
const SETTLE: Duration = Duration::from_secs(5);

// --- observation helpers -------------------------------------------------------

async fn listed_mcp_connected(client: &mut Connection, window_id: &str) -> Option<bool> {
    call_list_windows(client)
        .await
        .into_iter()
        .find(|w| w.window_id == window_id)
        .unwrap_or_else(|| panic!("window {window_id} is missing from list_windows"))
        .mcp_connected
}

async fn status_mcp_connected(client: &mut Connection, window_id: &str) -> Option<bool> {
    send(
        client,
        &ClientMessage::GetWindowStatus {
            window_id: window_id.to_string(),
        },
    )
    .await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::WindowStatus { mcp_connected, .. } => mcp_connected,
        other => panic!("expected window_status, got {other:?}"),
    }
}

/// Asserts both read paths report `expected` for `window_id` right now.
async fn assert_mcp_connected(observer: &mut Connection, window_id: &str, expected: bool) {
    assert_eq!(
        listed_mcp_connected(observer, window_id).await,
        Some(expected),
        "list_windows must report mcp_connected = Some({expected}) for {window_id}"
    );
    assert_eq!(
        status_mcp_connected(observer, window_id).await,
        Some(expected),
        "get_window_status must report mcp_connected = Some({expected}) for {window_id}"
    );
}

/// Polls both read paths until they report `expected`, because a closed
/// connection is only noticed by the daemon asynchronously.
async fn wait_for_mcp_connected(observer: &mut Connection, window_id: &str, expected: bool) {
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        let listed = listed_mcp_connected(observer, window_id).await;
        let status = status_mcp_connected(observer, window_id).await;
        if listed == Some(expected) && status == Some(expected) {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for mcp_connected = Some({expected}) on {window_id} \
                 (list_windows: {listed:?}, get_window_status: {status:?})"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Keeps reading until the daemon closes `client`, so a later assertion does
/// not race the daemon's own teardown of the connection.
async fn expect_closed_by_daemon(client: &mut Connection) {
    let mut buf = [0u8; 256];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "the daemon never closed the connection"
        );
        let n = tokio::time::timeout(remaining, client.read(&mut buf))
            .await
            .expect("the daemon never closed the connection")
            .expect("reading a connection the daemon closed must report EOF, not an error");
        if n == 0 {
            return;
        }
    }
}

async fn send_identify(client: &mut Connection, window_id: &str, token: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::Identify {
            window_id: window_id.to_string(),
            token: token.to_string(),
        },
    )
    .await;
    read_response(client, Duration::from_secs(5)).await
}

// --- report helpers ------------------------------------------------------------

async fn call_wait_for_report(client: &mut Connection, timeout_sec: u64) -> DaemonMessage {
    send(client, &ClientMessage::WaitForReport { timeout_sec }).await;
    read_response(client, Duration::from_secs(timeout_sec + 5)).await
}

fn expect_reports(
    msg: DaemonMessage,
) -> (
    Vec<yagura_protocol::ReceivedReport>,
    Option<Vec<AwaitingWindow>>,
) {
    match msg {
        DaemonMessage::Reports { reports, awaiting } => (reports, awaiting),
        other => panic!("expected reports, got {other:?}"),
    }
}

fn awaiting_ids(awaiting: &[AwaitingWindow]) -> BTreeSet<String> {
    awaiting.iter().map(|w| w.window_id.clone()).collect()
}

fn awaiting_entry<'a>(awaiting: &'a [AwaitingWindow], window_id: &str) -> &'a AwaitingWindow {
    awaiting
        .iter()
        .find(|w| w.window_id == window_id)
        .unwrap_or_else(|| panic!("window {window_id} is missing from awaiting {awaiting:?}"))
}

async fn call_send_report(client: &mut Connection, summary: &str) {
    send(
        client,
        &ClientMessage::SendReport {
            outcome: ReportOutcome::Succeeded,
            summary: summary.to_string(),
            path: None,
        },
    )
    .await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::ReportSent { .. } => {}
        other => panic!("expected report_sent, got {other:?}"),
    }
}

// --- plan helpers ----------------------------------------------------------------

fn ledger_config(dir: &Path) -> DaemonConfig {
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_profile(
        &profiles_dir,
        "presence-task-profile",
        &ready_child_profile_source("Presence Task Profile"),
    );
    let mut config = approver_base_config();
    config.approval_policy = Some(policy(&["open_window"]));
    config.store_db_path_resolved = Some(dir.join("plan.db"));
    config.profiles_dir = Some(profiles_dir);
    // This file measures report-destination presence, not the review gate:
    // the plan created here must not start pending.
    config.plan_approval_required = false;
    config
}

/// Reads the next reply that is not a `plan_changed` push.
async fn read_ledger_reply(client: &mut Connection) -> DaemonMessage {
    loop {
        let frame = read_response(client, Duration::from_secs(10)).await;
        if !matches!(frame, DaemonMessage::PlanChanged { .. }) {
            return frame;
        }
    }
}

async fn create_plan(client: &mut Connection, task_ids: &[&str]) -> PlanInfo {
    let tasks = task_ids
        .iter()
        .map(|task_id| PlanTaskSpec {
            task_id: (*task_id).to_string(),
            title: "do the thing".to_string(),
            role: None,
            profile: Some("presence-task-profile".to_string()),
            model: None,
            effort: None,
            depends_on: Vec::new(),
            writes: vec![format!("{task_id}.txt")],
            instruction: None,
        })
        .collect();
    send(
        client,
        &ClientMessage::CreatePlan {
            title: "plan".to_string(),
            tasks,
        },
    )
    .await;
    match read_ledger_reply(client).await {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

async fn open_task_window(client: &mut Connection, plan_id: &str, task_id: &str) -> String {
    send(
        client,
        &ClientMessage::OpenTaskWindow {
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
            name: None,
        },
    )
    .await;
    match read_ledger_reply(client).await {
        DaemonMessage::TaskWindowOpened { window_id, .. } => window_id,
        other => panic!("expected task_window_opened, got {other:?}"),
    }
}

async fn set_plan_owner(client: &mut Connection, plan_id: &str, window_id: &str) {
    send(
        client,
        &ClientMessage::SetPlanOwner {
            plan_id: plan_id.to_string(),
            window_id: window_id.to_string(),
        },
    )
    .await;
    match read_ledger_reply(client).await {
        DaemonMessage::PlanDetail { .. } => {}
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

// --- An identified connection marks its window ----------------------------

#[tokio::test]
async fn an_identified_connection_marks_its_window_until_the_client_disconnects() {
    let h = Harness::start("mcp-presence-client-close", approver_base_config()).await;
    let window_id = h.open(echo_child_command(), None).await;
    let mut observer = h.connect().await;

    assert_mcp_connected(&mut observer, &window_id, false).await;

    let mut bridge = h.connect().await;
    identify(&mut bridge, &window_id).await;
    assert_mcp_connected(&mut observer, &window_id, true).await;

    drop(bridge);
    wait_for_mcp_connected(&mut observer, &window_id, false).await;
}

#[tokio::test]
async fn an_identified_connection_the_daemon_disconnects_stops_marking_its_window() {
    let h = Harness::start("mcp-presence-daemon-close", approver_base_config()).await;
    let window_id = h.open(echo_child_command(), None).await;
    let mut observer = h.connect().await;

    let mut bridge = h.connect().await;
    identify(&mut bridge, &window_id).await;
    assert_mcp_connected(&mut observer, &window_id, true).await;

    // Repeated credential failures make the daemon end the connection itself.
    for _ in 0..3 {
        expect_error(
            send_identify(&mut bridge, &window_id, WRONG_TOKEN).await,
            "auth_failed",
        );
    }
    expect_closed_by_daemon(&mut bridge).await;

    wait_for_mcp_connected(&mut observer, &window_id, false).await;
}

// --- The mark counts connections, not identify calls -----------------------

#[tokio::test]
async fn a_window_stays_marked_until_its_last_identified_connection_closes() {
    let h = Harness::start("mcp-presence-two-connections", approver_base_config()).await;
    let window_id = h.open(echo_child_command(), None).await;
    let mut observer = h.connect().await;

    let mut first = h.connect().await;
    identify(&mut first, &window_id).await;
    let mut second = h.connect().await;
    identify(&mut second, &window_id).await;
    assert_mcp_connected(&mut observer, &window_id, true).await;

    drop(first);
    // Give the daemon the same time it gets to notice a close elsewhere, then
    // require the remaining connection to still count.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_mcp_connected(&mut observer, &window_id, true).await;

    drop(second);
    wait_for_mcp_connected(&mut observer, &window_id, false).await;
}

#[tokio::test]
async fn identifying_the_same_connection_twice_counts_it_once() {
    let h = Harness::start("mcp-presence-reidentify", approver_base_config()).await;
    let window_id = h.open(echo_child_command(), None).await;
    let mut observer = h.connect().await;

    let mut bridge = h.connect().await;
    identify(&mut bridge, &window_id).await;
    identify(&mut bridge, &window_id).await;
    assert_mcp_connected(&mut observer, &window_id, true).await;

    drop(bridge);
    wait_for_mcp_connected(&mut observer, &window_id, false).await;
}

// --- A refused identify does not mark the window ----------------------------

#[tokio::test]
async fn an_identify_with_a_wrong_or_malformed_token_does_not_mark_the_window() {
    let h = Harness::start("mcp-presence-refused", approver_base_config()).await;
    let window_id = h.open(echo_child_command(), None).await;
    let mut observer = h.connect().await;

    let mut wrong = h.connect().await;
    expect_error(
        send_identify(&mut wrong, &window_id, WRONG_TOKEN).await,
        "auth_failed",
    );
    let mut malformed = h.connect().await;
    expect_error(
        send_identify(&mut malformed, &window_id, "not-a-token").await,
        "auth_failed",
    );

    assert_mcp_connected(&mut observer, &window_id, false).await;

    // Both refused connections are still open here, so only the refusal is judged.
    drop(wrong);
    drop(malformed);
}

// --- An empty wait_for_report lists the windows still expected to report ----

#[tokio::test]
async fn an_empty_wait_for_report_lists_the_callers_live_children_with_their_mark() {
    let h = Harness::start("mcp-presence-awaiting-opener", approver_base_config()).await;

    let parent_window = h.open(echo_child_command(), None).await;
    let mut parent = h.connect().await;
    identify(&mut parent, &parent_window).await;

    let bridged_child = call_open_window(&mut parent, echo_child_command()).await;
    let silent_child = call_open_window(&mut parent, echo_child_command()).await;
    let exited_child = call_open_window(&mut parent, exit_with_code_command("0")).await;

    let other_window = h.open(echo_child_command(), None).await;
    let mut other = h.connect().await;
    identify(&mut other, &other_window).await;
    let other_child = call_open_window(&mut other, echo_child_command()).await;

    let mut child_bridge = h.connect().await;
    identify(&mut child_bridge, &bridged_child).await;

    let mut observer = h.connect().await;
    wait_until_status(
        &mut observer,
        &exited_child,
        WindowStatus::Exited,
        Duration::from_secs(10),
    )
    .await;
    let listed = call_list_windows(&mut observer).await;

    for timeout_sec in [0, 1] {
        let (reports, awaiting) =
            expect_reports(call_wait_for_report(&mut parent, timeout_sec).await);
        assert!(reports.is_empty());
        let awaiting = awaiting.unwrap_or_else(|| {
            panic!("an empty answer (timeout_sec = {timeout_sec}) must carry awaiting")
        });
        assert_eq!(
            awaiting_ids(&awaiting),
            BTreeSet::from([bridged_child.clone(), silent_child.clone()]),
            "awaiting (timeout_sec = {timeout_sec}) must hold exactly the caller's live \
             children: not another parent's child ({other_child}), not an exited one \
             ({exited_child})"
        );
        for (child, mark) in [(&bridged_child, true), (&silent_child, false)] {
            let entry = awaiting_entry(&awaiting, child);
            let info = listed
                .iter()
                .find(|w| &w.window_id == child)
                .expect("the child must be listed");
            assert_eq!(entry.name, info.name);
            assert_ne!(entry.status, WindowStatus::Exited);
            assert_eq!(entry.mcp_connected, Some(mark));
        }
    }
}

#[tokio::test]
async fn a_wait_for_report_that_returns_reports_carries_no_awaiting() {
    let h = Harness::start("mcp-presence-awaiting-none", approver_base_config()).await;

    let parent_window = h.open(echo_child_command(), None).await;
    let mut parent = h.connect().await;
    identify(&mut parent, &parent_window).await;
    let child = call_open_window(&mut parent, echo_child_command()).await;
    let _second_child = call_open_window(&mut parent, echo_child_command()).await;

    let mut child_conn = h.connect().await;
    identify(&mut child_conn, &child).await;
    call_send_report(&mut child_conn, "done").await;

    let (reports, awaiting) = expect_reports(call_wait_for_report(&mut parent, 0).await);
    assert_eq!(reports.len(), 1);
    assert_eq!(awaiting, None);
}

#[tokio::test]
async fn awaiting_follows_the_report_destination_for_task_windows() {
    let dir = unique_temp_dir("mcp-presence-awaiting-task");
    let (h, _locks) =
        Harness::start_with_ledger("mcp-presence-awaiting-task", ledger_config(&dir)).await;

    let first_owner_window = h.open(echo_child_command(), None).await;
    let mut first_owner = h.connect().await;
    identify(&mut first_owner, &first_owner_window).await;
    let plan = create_plan(&mut first_owner, &["t1"]).await;
    let task_window = open_task_window(&mut first_owner, &plan.plan_id, "t1").await;

    let second_owner_window = h.open(echo_child_command(), None).await;
    let mut second_owner = h.connect().await;
    identify(&mut second_owner, &second_owner_window).await;

    let (_, awaiting) = expect_reports(call_wait_for_report(&mut first_owner, 0).await);
    let awaiting = awaiting.expect("an empty answer must carry awaiting");
    assert_eq!(
        awaiting_ids(&awaiting),
        BTreeSet::from([task_window.clone()]),
        "the plan owner must be told its task window is expected to report"
    );
    assert_eq!(
        awaiting_entry(&awaiting, &task_window).mcp_connected,
        Some(false)
    );

    // Nominating another owner moves the task window's report destination
    // away from the window that opened it.
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    set_plan_owner(&mut approver, &plan.plan_id, &second_owner_window).await;

    let (_, awaiting) = expect_reports(call_wait_for_report(&mut second_owner, 0).await);
    let awaiting = awaiting.expect("an empty answer must carry awaiting");
    assert_eq!(
        awaiting_ids(&awaiting),
        BTreeSet::from([task_window.clone()]),
        "the nominated owner must be told the task window reports to it"
    );

    let (_, awaiting) = expect_reports(call_wait_for_report(&mut first_owner, 0).await);
    let awaiting = awaiting.expect("an empty answer must carry awaiting");
    assert!(
        awaiting.is_empty(),
        "the superseded owner must not be told to expect the task window's report: {awaiting:?}"
    );
}
