#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the daemon's task-report path over IPC (`send_report` /
//! `wait_for_report`) -- opener and `task_origin` recording, `resolve_parent`,
//! the per-window inbox and its lifetime, and `wait_for_report`'s wait /
//! direct-hand-off behavior. Driven through a real `Harness`, never a real
//! vendor CLI. The notice text this same inbox eventually triggers is
//! measured in `ipc_report_notice.rs`, not here.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{
    Harness, approver_base_config, call_open_window, call_send_handoff_from_profile_within,
    empty_lookup, expect_error, expect_handoff_sent, handoff_profile_config, identify,
    launch_prompt_profile_source, policy, read_response, register_approver, send, unique_temp_dir,
    write_profile,
};

use yagura_daemon::DaemonConfig;
use yagura_daemon::ipc::report::REPORT_INBOX_MAX;
use yagura_protocol::{
    ClientMessage, DaemonMessage, PlanInfo, PlanTaskSpec, REPORT_PATH_MAX_BYTES,
    REPORT_SUMMARY_MAX_BYTES, ReceivedReport, ReportOutcome, ReportTaskRef, WindowStatus,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::{echo_child_command, ready_child_profile_source};

// --- config builders ---------------------------------------------------------

/// A window-opening/closing/handoff-capable config, no plan ledger wired in:
/// the baseline for every non-task case, none of which needs a plan.
fn base_config() -> DaemonConfig {
    DaemonConfig {
        approval_policy: Some(common::policy_opening(&["close_window", "send_handoff"])),
        ..approver_base_config()
    }
}

/// [`base_config`] plus a resolved `profiles_dir`, for the profile-addressed
/// opener case.
fn profile_open_config(profiles_dir: PathBuf) -> DaemonConfig {
    DaemonConfig {
        profiles_dir: Some(profiles_dir),
        ..base_config()
    }
}

/// A plan-ledger config for the task-binding cases,
/// mirroring `ipc_plan_assign.rs`'s own `assign_config`. `plan_approval_required`
/// is off: this file measures the report path, not the review gate, so a
/// task-binding case's `open_task_window` must not be refused.
fn report_ledger_config(dir: &Path, profiles_dir: PathBuf, auto_approve: &[&str]) -> DaemonConfig {
    let mut config = approver_base_config();
    config.approval_policy = Some(common::policy_opening(auto_approve));
    config.store_db_path_resolved = Some(dir.join("plan.db"));
    config.profiles_dir = Some(profiles_dir);
    config.plan_approval_required = false;
    config
}

// --- plan/task fixtures -------------------------------------------------------

fn task(task_id: &str) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: task_id.to_string(),
        title: "do the thing".to_string(),
        role: None,
        profile: None,
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: None,
    }
}

fn task_with_profile(task_id: &str, profile: &str) -> PlanTaskSpec {
    PlanTaskSpec {
        profile: Some(profile.to_string()),
        ..task(task_id)
    }
}

/// A launch profile whose `[command]` is `ready_child`, absolute-pathed (no
/// program-lookup resolution needed) and never self-exiting, so a task
/// window bound to it stays alive for the whole test.
fn write_plain_profile(dir: &Path, id: &str, display_name: &str) {
    write_profile(dir, id, &ready_child_profile_source(display_name));
}

// --- connection/window helpers -------------------------------------------------

/// Opens a plain `echo_child` window through the registry directly (bypassing
/// IPC, like `Harness::open`) and identifies `client` as it. Used wherever a
/// window's own `opener` does not matter to the case (an owner, an
/// already-open nomination target, ...).
async fn open_and_identify(h: &Harness, client: &mut Connection) -> String {
    let window_id = h.open(echo_child_command(), None).await;
    identify(client, &window_id).await;
    window_id
}

async fn approver_connection(h: &Harness) -> Connection {
    let mut client = h.connect().await;
    register_approver(&mut client).await;
    client
}

/// Reads the next *response* frame, skipping `plan_changed` pushes the same
/// way `ipc_plan_assign.rs`'s own helper does: a connection that is also a
/// registered approver may be pushed to while its own request is in flight.
async fn read_ledger_reply(client: &mut Connection, timeout: Duration) -> DaemonMessage {
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

async fn create_plan(
    client: &mut Connection,
    title: &str,
    plan_tasks: Vec<PlanTaskSpec>,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::CreatePlan {
            title: title.to_string(),
            tasks: plan_tasks,
        },
    )
    .await;
    read_ledger_reply(client, Duration::from_secs(5)).await
}

async fn get_plan(client: &mut Connection, plan_id: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::GetPlan {
            plan_id: plan_id.to_string(),
        },
    )
    .await;
    read_ledger_reply(client, Duration::from_secs(5)).await
}

async fn update_plan(
    client: &mut Connection,
    plan_id: &str,
    plan_tasks: Vec<PlanTaskSpec>,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::UpdatePlan {
            plan_id: plan_id.to_string(),
            title: None,
            tasks: plan_tasks,
        },
    )
    .await;
    read_ledger_reply(client, Duration::from_secs(5)).await
}

async fn delete_plan(client: &mut Connection, plan_id: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::DeletePlan {
            plan_id: plan_id.to_string(),
        },
    )
    .await;
    read_ledger_reply(client, Duration::from_secs(5)).await
}

async fn set_plan_owner(client: &mut Connection, plan_id: &str, window_id: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::SetPlanOwner {
            plan_id: plan_id.to_string(),
            window_id: window_id.to_string(),
        },
    )
    .await;
    read_ledger_reply(client, Duration::from_secs(5)).await
}

async fn open_task_window(client: &mut Connection, plan_id: &str, task_id: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::OpenTaskWindow {
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
            name: None,
        },
    )
    .await;
    read_ledger_reply(client, Duration::from_secs(10)).await
}

fn expect_plan_detail(msg: DaemonMessage) -> PlanInfo {
    match msg {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

#[allow(clippy::type_complexity)]
fn expect_task_window_opened(
    msg: DaemonMessage,
) -> (String, String, String, yagura_protocol::ResolvedAssignment) {
    match msg {
        DaemonMessage::TaskWindowOpened {
            plan_id,
            task_id,
            window_id,
            assignment,
            ..
        } => (plan_id, task_id, window_id, assignment),
        other => panic!("expected task_window_opened, got {other:?}"),
    }
}

fn task_info<'a>(plan: &'a PlanInfo, task_id: &str) -> &'a yagura_protocol::PlanTaskInfo {
    plan.tasks
        .iter()
        .find(|t| t.task_id == task_id)
        .unwrap_or_else(|| panic!("task {task_id:?} not found in plan {plan:?}"))
}

async fn create_ok(
    client: &mut Connection,
    title: &str,
    plan_tasks: Vec<PlanTaskSpec>,
) -> PlanInfo {
    expect_plan_detail(create_plan(client, title, plan_tasks).await)
}

async fn expect_plan_changed(client: &mut Connection, plan_id: &str) {
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::PlanChanged { plan_id: pushed } => assert_eq!(pushed, plan_id),
        other => panic!("expected plan_changed, got {other:?}"),
    }
}

async fn close_window(closer: &mut Connection, window_id: &str) {
    send(
        closer,
        &ClientMessage::CloseWindow {
            window_id: window_id.to_string(),
            force: None,
        },
    )
    .await;
    match read_response(closer, Duration::from_secs(10)).await {
        DaemonMessage::WindowClosed { window_id: closed } => assert_eq!(closed, window_id),
        other => panic!("expected window_closed, got {other:?}"),
    }
}

// --- report wire round trips ---------------------------------------------------

async fn call_send_report(
    client: &mut Connection,
    outcome: ReportOutcome,
    summary: &str,
    path: Option<&str>,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::SendReport {
            outcome,
            summary: summary.to_string(),
            path: path.map(str::to_string),
        },
    )
    .await;
    read_response(client, Duration::from_secs(5)).await
}

async fn call_wait_for_report(client: &mut Connection, timeout_sec: u64) -> DaemonMessage {
    call_wait_for_report_with_timeout(client, timeout_sec, Duration::from_secs(5)).await
}

async fn call_wait_for_report_with_timeout(
    client: &mut Connection,
    timeout_sec: u64,
    read_timeout: Duration,
) -> DaemonMessage {
    send(client, &ClientMessage::WaitForReport { timeout_sec }).await;
    read_response(client, read_timeout).await
}

fn expect_report_sent(msg: DaemonMessage) -> (Option<String>, Option<ReportTaskRef>) {
    match msg {
        DaemonMessage::ReportSent { delivered_to, task } => (delivered_to, task),
        other => panic!("expected report_sent, got {other:?}"),
    }
}

fn expect_reports(msg: DaemonMessage) -> Vec<ReceivedReport> {
    match msg {
        DaemonMessage::Reports {
            reports,
            awaiting: _,
        } => reports,
        other => panic!("expected reports, got {other:?}"),
    }
}

fn unix_ms_now() -> u64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the Unix epoch");
    u64::try_from(elapsed.as_millis()).expect("milliseconds fit in u64")
}

// --- Who becomes a window's opener ---------------------------------------

#[tokio::test]
async fn a_window_opened_by_an_identified_connection_becomes_its_opener_and_receives_its_report() {
    let h = Harness::start("report-opener-basic", base_config()).await;

    let window_a = h.open(echo_child_command(), None).await;
    let mut a = h.connect().await;
    identify(&mut a, &window_a).await;
    let window_b = call_open_window(&mut a, echo_child_command()).await;
    let mut b = h.connect().await;
    identify(&mut b, &window_b).await;

    let sent_after = unix_ms_now();
    let reply = call_send_report(
        &mut b,
        ReportOutcome::Succeeded,
        "build finished",
        Some("C:\\out\\log.txt"),
    )
    .await;
    let answered_before = unix_ms_now();
    let (delivered_to, task) = expect_report_sent(reply);
    assert_eq!(delivered_to.as_deref(), Some(window_a.as_str()));
    assert_eq!(task, None);

    let reports = expect_reports(call_wait_for_report(&mut a, 0).await);
    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert_eq!(report.from_window_id, window_b);
    assert_eq!(report.outcome, ReportOutcome::Succeeded);
    assert_eq!(report.summary, "build finished");
    assert_eq!(report.path.as_deref(), Some("C:\\out\\log.txt"));
    assert_eq!(report.task, None);
    assert!(
        (sent_after..=answered_before).contains(&report.at_ms),
        "at_ms {} must be the wall-clock time the report was received",
        report.at_ms
    );
}

#[tokio::test]
async fn a_window_opened_from_a_profile_by_an_identified_connection_becomes_its_opener() {
    let dir = unique_temp_dir("report-opener-profile-open");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "report-profile", "Report Profile");
    let h = Harness::start(
        "report-opener-profile-open",
        profile_open_config(profiles_dir),
    )
    .await;

    let window_a = h.open(echo_child_command(), None).await;
    let mut a = h.connect().await;
    identify(&mut a, &window_a).await;

    send(
        &mut a,
        &ClientMessage::OpenWindowFromProfile {
            profile: "report-profile".to_string(),
            cwd: None,
            name: None,
            model: None,
            effort: None,
            on_name_taken: Default::default(),
        },
    )
    .await;
    let window_b = match read_response(&mut a, Duration::from_secs(5)).await {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected window_opened, got {other:?}"),
    };

    let mut b = h.connect().await;
    identify(&mut b, &window_b).await;
    let reply = call_send_report(&mut b, ReportOutcome::Succeeded, "profile-opened", None).await;
    let (delivered_to, _) = expect_report_sent(reply);
    assert_eq!(delivered_to.as_deref(), Some(window_a.as_str()));
}

#[tokio::test]
async fn a_window_opened_via_a_profile_addressed_send_handoff_becomes_its_opener() {
    let tree = TempDir::new("yagura-cli-test", "report-opener-handoff-profile");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "report-handoff-profile",
        &launch_prompt_profile_source("Report Handoff Profile", None, ""),
    );
    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    let h = Harness::start("report-opener-handoff-profile", config).await;

    let window_a = h.open(echo_child_command(), None).await;
    let mut a = h.connect().await;
    identify(&mut a, &window_a).await;

    let msg = call_send_handoff_from_profile_within(
        &mut a,
        "report-handoff-profile",
        None,
        None,
        "src/foo.rs",
        "look here",
        Duration::from_secs(20),
    )
    .await;
    let (window_b, _opened, _delivery) = expect_handoff_sent(msg);

    let mut b = h.connect().await;
    identify(&mut b, &window_b).await;
    let reply = call_send_report(&mut b, ReportOutcome::Succeeded, "handoff-opened", None).await;
    let (delivered_to, _) = expect_report_sent(reply);
    assert_eq!(delivered_to.as_deref(), Some(window_a.as_str()));

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_window_opened_by_an_approver_or_external_connection_has_no_parent() {
    let h = Harness::start("report-opener-scope", base_config()).await;

    let mut approver = approver_connection(&h).await;
    let window_by_approver = call_open_window(&mut approver, echo_child_command()).await;
    let mut b1 = h.connect().await;
    identify(&mut b1, &window_by_approver).await;
    let reply = call_send_report(&mut b1, ReportOutcome::Succeeded, "no parent", None).await;
    expect_error(reply, "invalid_state");

    let mut external = h.connect().await;
    let window_by_external = call_open_window(&mut external, echo_child_command()).await;
    let mut b2 = h.connect().await;
    identify(&mut b2, &window_by_external).await;
    let reply = call_send_report(&mut b2, ReportOutcome::Succeeded, "no parent", None).await;
    expect_error(reply, "invalid_state");
}

// --- Authorization scope ------------------------------------------------

#[tokio::test]
async fn send_report_and_wait_for_report_are_refused_for_external_and_approver_connections() {
    let h = Harness::start("report-authz-scope", base_config()).await;

    let mut external = h.connect().await;
    expect_error(
        call_send_report(
            &mut external,
            ReportOutcome::Succeeded,
            "not a window",
            None,
        )
        .await,
        "auth_failed",
    );
    expect_error(call_wait_for_report(&mut external, 0).await, "auth_failed");

    let mut approver = approver_connection(&h).await;
    expect_error(
        call_send_report(
            &mut approver,
            ReportOutcome::Succeeded,
            "not a window",
            None,
        )
        .await,
        "auth_failed",
    );
    expect_error(call_wait_for_report(&mut approver, 0).await, "auth_failed");
}

// --- Argument validation -------------------------------------------------

#[tokio::test]
async fn send_report_rejects_a_malformed_summary_or_path_before_ever_delivering_it() {
    let h = Harness::start("report-validation", base_config()).await;
    let window_a = h.open(echo_child_command(), None).await;
    let mut a = h.connect().await;
    identify(&mut a, &window_a).await;
    let window_b = call_open_window(&mut a, echo_child_command()).await;
    let mut b = h.connect().await;
    identify(&mut b, &window_b).await;

    let too_long_summary = "x".repeat(REPORT_SUMMARY_MAX_BYTES + 1);
    let bad_summaries: [&str; 4] = [
        "",
        "line one\nline two",
        "poison-\u{7}-value",
        &too_long_summary,
    ];
    for summary in bad_summaries {
        let message = expect_error(
            call_send_report(&mut b, ReportOutcome::Succeeded, summary, None).await,
            "invalid_argument",
        );
        assert!(
            !message.contains("poison-"),
            "a rejected summary must never be echoed back, got {message:?}"
        );
    }

    let too_long_path = "x".repeat(REPORT_PATH_MAX_BYTES + 1);
    let bad_paths: [&str; 3] = ["", "one\ntwo", &too_long_path];
    for path in bad_paths {
        expect_error(
            call_send_report(
                &mut b,
                ReportOutcome::Succeeded,
                "a fine summary",
                Some(path),
            )
            .await,
            "invalid_argument",
        );
    }
}

// --- Opener closed, non-task window --------------------------------------

#[tokio::test]
async fn a_non_task_windows_report_is_refused_once_its_opener_has_closed() {
    let h = Harness::start("report-opener-closed", base_config()).await;
    let window_a = h.open(echo_child_command(), None).await;
    let mut a = h.connect().await;
    identify(&mut a, &window_a).await;
    let window_b = call_open_window(&mut a, echo_child_command()).await;
    let mut b = h.connect().await;
    identify(&mut b, &window_b).await;

    let mut approver = approver_connection(&h).await;
    close_window(&mut approver, &window_a).await;

    let reply = call_send_report(&mut b, ReportOutcome::Succeeded, "opener is gone", None).await;
    expect_error(reply, "invalid_state");
}

// --- Task-bound windows report to the plan's owner --------------------------

#[tokio::test]
async fn a_bound_windows_report_reaches_the_plans_alive_owner_and_lands_on_the_task() {
    let dir = unique_temp_dir("report-task-basic");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "report-task-profile", "Report Task Profile");
    let (h, _locks) = Harness::start_with_ledger(
        "report-task-basic",
        report_ledger_config(&dir, profiles_dir, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    let owner_window = open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "report-task-profile")],
    )
    .await;
    let (_, _, bound_window, _) =
        expect_task_window_opened(open_task_window(&mut owner, &plan.plan_id, "t1").await);
    let mut bound = h.connect().await;
    identify(&mut bound, &bound_window).await;

    let mut approver = approver_connection(&h).await;

    let reply = call_send_report(
        &mut bound,
        ReportOutcome::Succeeded,
        "the build passed",
        Some("C:\\out\\result.txt"),
    )
    .await;
    let (delivered_to, task) = expect_report_sent(reply);
    assert_eq!(delivered_to.as_deref(), Some(owner_window.as_str()));
    assert_eq!(
        task,
        Some(ReportTaskRef {
            plan_id: plan.plan_id.clone(),
            task_id: "t1".to_string(),
        })
    );

    expect_plan_changed(&mut approver, &plan.plan_id).await;

    let reports = expect_reports(call_wait_for_report(&mut owner, 0).await);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].from_window_id, bound_window);
    assert_eq!(reports[0].outcome, ReportOutcome::Succeeded);
    assert_eq!(reports[0].summary, "the build passed");
    assert_eq!(reports[0].path.as_deref(), Some("C:\\out\\result.txt"));
    assert_eq!(
        reports[0].task,
        Some(ReportTaskRef {
            plan_id: plan.plan_id.clone(),
            task_id: "t1".to_string(),
        })
    );
    assert!(reports[0].at_ms > 0);

    let detail = expect_plan_detail(get_plan(&mut owner, &plan.plan_id).await);
    let report = task_info(&detail, "t1")
        .report
        .as_ref()
        .expect("t1 must carry the recorded report");
    assert_eq!(report.outcome, ReportOutcome::Succeeded);
    assert_eq!(report.summary, "the build passed");
    assert_eq!(report.path.as_deref(), Some("C:\\out\\result.txt"));
    assert!(report.reported_at > 0);
}

#[tokio::test]
async fn a_bound_windows_report_still_lands_on_the_task_once_its_owner_has_closed() {
    let dir = unique_temp_dir("report-task-dead-owner");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "report-task-profile", "Report Task Profile");
    let (h, _locks) = Harness::start_with_ledger(
        "report-task-dead-owner",
        report_ledger_config(&dir, profiles_dir, &["close_window"]),
    )
    .await;

    let mut owner = h.connect().await;
    let owner_window = open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "report-task-profile")],
    )
    .await;
    let (_, _, bound_window, _) =
        expect_task_window_opened(open_task_window(&mut owner, &plan.plan_id, "t1").await);
    let mut bound = h.connect().await;
    identify(&mut bound, &bound_window).await;

    let mut approver = approver_connection(&h).await;
    close_window(&mut approver, &owner_window).await;

    let reply =
        call_send_report(&mut bound, ReportOutcome::Failed, "the owner is gone", None).await;
    let (delivered_to, task) = expect_report_sent(reply);
    assert_eq!(delivered_to, None);
    assert_eq!(task.map(|t| t.task_id), Some("t1".to_string()));

    let detail = expect_plan_detail(get_plan(&mut bound, &plan.plan_id).await);
    let report = task_info(&detail, "t1")
        .report
        .as_ref()
        .expect("the report must still be recorded even with no live owner to deliver to");
    assert_eq!(report.outcome, ReportOutcome::Failed);
    assert_eq!(report.summary, "the owner is gone");
}

#[tokio::test]
async fn a_bound_windows_report_follows_an_owner_nomination_not_the_windows_own_opener() {
    let dir = unique_temp_dir("report-task-renominate");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "report-task-profile", "Report Task Profile");
    let (h, _locks) = Harness::start_with_ledger(
        "report-task-renominate",
        report_ledger_config(&dir, profiles_dir, &[]),
    )
    .await;

    let mut owner_a = h.connect().await;
    open_and_identify(&h, &mut owner_a).await;
    let plan = create_ok(
        &mut owner_a,
        "plan",
        vec![task_with_profile("t1", "report-task-profile")],
    )
    .await;
    let (_, _, bound_window, _) =
        expect_task_window_opened(open_task_window(&mut owner_a, &plan.plan_id, "t1").await);
    let mut bound = h.connect().await;
    identify(&mut bound, &bound_window).await;

    let mut owner_b = h.connect().await;
    let window_b = open_and_identify(&h, &mut owner_b).await;

    let mut approver = approver_connection(&h).await;
    expect_plan_detail(set_plan_owner(&mut approver, &plan.plan_id, &window_b).await);

    let reply = call_send_report(
        &mut bound,
        ReportOutcome::Succeeded,
        "after nomination",
        None,
    )
    .await;
    let (delivered_to, _) = expect_report_sent(reply);
    assert_eq!(delivered_to.as_deref(), Some(window_b.as_str()));

    let reports = expect_reports(call_wait_for_report(&mut owner_b, 0).await);
    assert_eq!(reports.len(), 1);
    let leftover = expect_reports(call_wait_for_report(&mut owner_a, 0).await);
    assert!(
        leftover.is_empty(),
        "the superseded owner must not have received the report"
    );
}

#[tokio::test]
async fn a_reports_stale_window_identity_is_refused_once_its_task_has_been_rebound() {
    let dir = unique_temp_dir("report-task-rebind-stale");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "report-task-profile", "Report Task Profile");
    let (h, _locks) = Harness::start_with_ledger(
        "report-task-rebind-stale",
        report_ledger_config(&dir, profiles_dir, &["close_window"]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "report-task-profile")],
    )
    .await;
    let (_, _, window_1, _) =
        expect_task_window_opened(open_task_window(&mut owner, &plan.plan_id, "t1").await);

    let mut approver = approver_connection(&h).await;
    close_window(&mut approver, &window_1).await;

    let (_, _, window_2, _) =
        expect_task_window_opened(open_task_window(&mut owner, &plan.plan_id, "t1").await);
    assert_ne!(window_1, window_2);

    let mut stale = h.connect().await;
    identify(&mut stale, &window_1).await;
    let reply = call_send_report(&mut stale, ReportOutcome::Failed, "stale attempt", None).await;
    expect_error(reply, "invalid_state");

    let detail = expect_plan_detail(get_plan(&mut owner, &plan.plan_id).await);
    assert!(task_info(&detail, "t1").report.is_none());

    let reports = expect_reports(call_wait_for_report(&mut owner, 0).await);
    assert!(reports.is_empty());
}

#[tokio::test]
async fn a_bound_windows_report_is_refused_once_its_task_was_dropped_from_the_plan() {
    let dir = unique_temp_dir("report-task-dropped");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "report-task-profile", "Report Task Profile");
    let (h, _locks) = Harness::start_with_ledger(
        "report-task-dropped",
        report_ledger_config(&dir, profiles_dir, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "report-task-profile")],
    )
    .await;
    let (_, _, bound_window, _) =
        expect_task_window_opened(open_task_window(&mut owner, &plan.plan_id, "t1").await);
    let mut bound = h.connect().await;
    identify(&mut bound, &bound_window).await;

    expect_plan_detail(update_plan(&mut owner, &plan.plan_id, vec![task("t2")]).await);

    let reply = call_send_report(&mut bound, ReportOutcome::Succeeded, "orphaned", None).await;
    expect_error(reply, "invalid_state");

    let reports = expect_reports(call_wait_for_report(&mut owner, 0).await);
    assert!(reports.is_empty());
}

#[tokio::test]
async fn a_bound_windows_report_is_refused_once_the_plan_is_deleted_and_leaves_no_lock_table_entry()
{
    let dir = unique_temp_dir("report-task-plan-deleted");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "report-task-profile", "Report Task Profile");
    let (h, locks) = Harness::start_with_ledger(
        "report-task-plan-deleted",
        report_ledger_config(&dir, profiles_dir, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "report-task-profile")],
    )
    .await;
    let (_, _, bound_window, _) =
        expect_task_window_opened(open_task_window(&mut owner, &plan.plan_id, "t1").await);
    let mut bound = h.connect().await;
    identify(&mut bound, &bound_window).await;

    let reply = delete_plan(&mut owner, &plan.plan_id).await;
    assert!(matches!(reply, DaemonMessage::PlanDeleted { .. }));
    let after_delete = locks.len();

    let reply = call_send_report(&mut bound, ReportOutcome::Succeeded, "too late", None).await;
    expect_error(reply, "invalid_state");
    assert_eq!(
        locks.len(),
        after_delete,
        "a report against a deleted plan must not leave a fresh lock-table entry behind"
    );
}

fn awaited_window_ids(msg: DaemonMessage) -> Vec<String> {
    match msg {
        DaemonMessage::Reports {
            reports,
            awaiting: Some(awaiting),
        } if reports.is_empty() => awaiting.into_iter().map(|w| w.window_id).collect(),
        other => panic!("expected an empty reports answer carrying awaiting, got {other:?}"),
    }
}

#[tokio::test]
async fn a_task_window_whose_plan_was_deleted_is_no_longer_awaited_by_the_window_that_opened_it() {
    let dir = unique_temp_dir("report-task-plan-deleted-awaiting");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "report-task-profile", "Report Task Profile");
    let (h, _locks) = Harness::start_with_ledger(
        "report-task-plan-deleted-awaiting",
        report_ledger_config(&dir, profiles_dir, &[]),
    )
    .await;

    // The owner is also the task window's opener, the window it would wrongly
    // fall back to.
    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "report-task-profile")],
    )
    .await;
    let (_, _, bound_window, _) =
        expect_task_window_opened(open_task_window(&mut owner, &plan.plan_id, "t1").await);
    assert_eq!(
        awaited_window_ids(call_wait_for_report(&mut owner, 0).await),
        vec![bound_window.clone()]
    );

    let reply = delete_plan(&mut owner, &plan.plan_id).await;
    assert!(matches!(reply, DaemonMessage::PlanDeleted { .. }));

    assert!(
        !awaited_window_ids(call_wait_for_report(&mut owner, 0).await).contains(&bound_window),
        "a task window whose report is refused must not be listed as one still to report"
    );
}

// --- Inbox size cap ---------------------------------------------------------

#[tokio::test]
async fn a_full_inbox_refuses_the_next_report_after_accepting_every_report_up_to_the_cap() {
    let h = Harness::start("report-inbox-full", base_config()).await;

    let window_a = h.open(echo_child_command(), None).await;
    let mut a = h.connect().await;
    identify(&mut a, &window_a).await;
    let window_b = call_open_window(&mut a, echo_child_command()).await;
    let mut b = h.connect().await;
    identify(&mut b, &window_b).await;

    for i in 0..REPORT_INBOX_MAX {
        let reply = call_send_report(
            &mut b,
            ReportOutcome::Succeeded,
            &format!("report {i}"),
            None,
        )
        .await;
        let (delivered_to, _) = expect_report_sent(reply);
        assert_eq!(delivered_to.as_deref(), Some(window_a.as_str()));
    }

    let reply = call_send_report(&mut b, ReportOutcome::Succeeded, "one too many", None).await;
    expect_error(reply, "resource_exhausted");
}

#[tokio::test]
async fn a_full_inbox_refuses_a_bound_windows_report_too_and_never_writes_it_to_the_ledger() {
    let dir = unique_temp_dir("report-inbox-full-task");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "report-task-profile", "Report Task Profile");
    let (h, _locks) = Harness::start_with_ledger(
        "report-inbox-full-task",
        report_ledger_config(&dir, profiles_dir, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "report-task-profile")],
    )
    .await;
    let (_, _, bound_window, _) =
        expect_task_window_opened(open_task_window(&mut owner, &plan.plan_id, "t1").await);
    let mut bound = h.connect().await;
    identify(&mut bound, &bound_window).await;

    let filler_window = call_open_window(&mut owner, echo_child_command()).await;
    let mut filler = h.connect().await;
    identify(&mut filler, &filler_window).await;
    for i in 0..REPORT_INBOX_MAX {
        expect_report_sent(
            call_send_report(
                &mut filler,
                ReportOutcome::Succeeded,
                &format!("filler {i}"),
                None,
            )
            .await,
        );
    }

    let reply = call_send_report(&mut bound, ReportOutcome::Succeeded, "task done", None).await;
    expect_error(reply, "resource_exhausted");

    let detail = expect_plan_detail(get_plan(&mut bound, &plan.plan_id).await);
    assert!(
        task_info(&detail, "t1").report.is_none(),
        "a report refused for a full inbox must not be written to the ledger either"
    );
}

// --- wait_for_report --------------------------------------------------------

#[tokio::test]
async fn wait_for_report_drains_every_waiting_report_and_leaves_the_inbox_empty() {
    let h = Harness::start("report-wait-drain", base_config()).await;
    let window_a = h.open(echo_child_command(), None).await;
    let mut a = h.connect().await;
    identify(&mut a, &window_a).await;
    let window_b = call_open_window(&mut a, echo_child_command()).await;
    let mut b = h.connect().await;
    identify(&mut b, &window_b).await;

    expect_report_sent(call_send_report(&mut b, ReportOutcome::Succeeded, "first", None).await);
    expect_report_sent(call_send_report(&mut b, ReportOutcome::Failed, "second", None).await);

    let reports = expect_reports(call_wait_for_report(&mut a, 0).await);
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].summary, "first");
    assert_eq!(reports[1].summary, "second");

    let reports = expect_reports(call_wait_for_report(&mut a, 0).await);
    assert!(reports.is_empty());
}

#[tokio::test]
async fn wait_for_report_returns_as_soon_as_a_report_arrives_and_empty_on_a_bare_timeout() {
    let h = Harness::start("report-wait-arrival", base_config()).await;
    let window_a = h.open(echo_child_command(), None).await;

    let mut idle = h.connect().await;
    identify(&mut idle, &window_a).await;
    let empty = expect_reports(
        call_wait_for_report_with_timeout(&mut idle, 1, Duration::from_secs(5)).await,
    );
    assert!(empty.is_empty());

    let window_b = call_open_window(&mut idle, echo_child_command()).await;
    let mut b = h.connect().await;
    identify(&mut b, &window_b).await;

    let mut waiter = h.connect().await;
    identify(&mut waiter, &window_a).await;
    let wait_handle = tokio::spawn(async move {
        expect_reports(
            call_wait_for_report_with_timeout(&mut waiter, 10, Duration::from_secs(15)).await,
        )
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    expect_report_sent(
        call_send_report(
            &mut b,
            ReportOutcome::Succeeded,
            "arrived while waiting",
            None,
        )
        .await,
    );

    let reports = wait_handle.await.expect("the waiting task must not panic");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].summary, "arrived while waiting");
}

#[tokio::test]
async fn wait_for_report_rejects_a_timeout_over_the_configured_maximum() {
    let mut config = base_config();
    config.wait_timeout_max_secs = 5;
    let h = Harness::start("report-wait-timeout-cap", config).await;
    let window_a = h.open(echo_child_command(), None).await;
    let mut a = h.connect().await;
    identify(&mut a, &window_a).await;

    let reply = call_wait_for_report(&mut a, 6).await;
    expect_error(reply, "invalid_argument");
}

#[tokio::test]
async fn wait_for_report_accepts_a_timeout_exactly_at_the_configured_maximum() {
    let mut config = base_config();
    config.wait_timeout_max_secs = 5;
    let h = Harness::start("report-wait-timeout-at-cap", config).await;
    let window_a = h.open(echo_child_command(), None).await;
    let mut a = h.connect().await;
    identify(&mut a, &window_a).await;
    let window_b = call_open_window(&mut a, echo_child_command()).await;
    let mut b = h.connect().await;
    identify(&mut b, &window_b).await;
    expect_report_sent(
        call_send_report(&mut b, ReportOutcome::Succeeded, "at the cap", None).await,
    );

    // The report is already queued, so the call returns without waiting.
    let reports = expect_reports(call_wait_for_report(&mut a, 5).await);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].summary, "at the cap");
}

#[tokio::test]
async fn wait_for_report_shares_its_connections_waiter_budget_with_wait_for_status() {
    let mut config = base_config();
    config.max_waiters_per_conn = 1;
    let h = Harness::start("report-wait-shared-budget", config).await;
    let window_a = h.open(echo_child_command(), None).await;
    let mut a = h.connect().await;
    identify(&mut a, &window_a).await;

    let unknown_window = h.open_unknown_window().await;
    send(
        &mut a,
        &ClientMessage::WaitForStatus {
            window_id: unknown_window,
            target_status: WindowStatus::WaitingInput,
            timeout_sec: 60,
        },
    )
    .await;
    // Frames on one connection are handled in order, so the pong proves the
    // wait above is already parked.
    send(&mut a, &ClientMessage::Ping).await;
    match read_response(&mut a, Duration::from_secs(5)).await {
        DaemonMessage::Pong => {}
        other => panic!("expected pong, got {other:?}"),
    }

    let reply = call_wait_for_report(&mut a, 1).await;
    expect_error(reply, "resource_exhausted");

    // A zero timeout never parks a wait, so it needs no budget.
    let reports = expect_reports(call_wait_for_report(&mut a, 0).await);
    assert!(reports.is_empty());
}

#[tokio::test]
async fn a_report_that_arrives_after_its_waiting_connection_disconnects_is_not_lost() {
    let h = Harness::start("report-wait-disconnect", base_config()).await;
    let window_a = h.open(echo_child_command(), None).await;
    let mut a1 = h.connect().await;
    identify(&mut a1, &window_a).await;
    let window_b = call_open_window(&mut a1, echo_child_command()).await;

    send(&mut a1, &ClientMessage::WaitForReport { timeout_sec: 30 }).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(a1);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut b = h.connect().await;
    identify(&mut b, &window_b).await;
    let (delivered_to, _) = expect_report_sent(
        call_send_report(&mut b, ReportOutcome::Succeeded, "after disconnect", None).await,
    );
    assert_eq!(delivered_to.as_deref(), Some(window_a.as_str()));

    let mut a2 = h.connect().await;
    identify(&mut a2, &window_a).await;
    let reports = expect_reports(call_wait_for_report(&mut a2, 0).await);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].summary, "after disconnect");
}

#[tokio::test]
async fn a_report_handed_directly_to_a_waiter_never_also_sits_in_the_inbox() {
    let h = Harness::start("report-wait-direct-handoff", base_config()).await;
    let window_a = h.open(echo_child_command(), None).await;
    let mut a1 = h.connect().await;
    identify(&mut a1, &window_a).await;
    let window_b = call_open_window(&mut a1, echo_child_command()).await;

    let wait_handle = tokio::spawn(async move {
        expect_reports(
            call_wait_for_report_with_timeout(&mut a1, 10, Duration::from_secs(15)).await,
        )
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut b = h.connect().await;
    identify(&mut b, &window_b).await;
    expect_report_sent(
        call_send_report(&mut b, ReportOutcome::Succeeded, "direct handoff", None).await,
    );

    let reports = wait_handle.await.expect("the waiting task must not panic");
    assert_eq!(reports.len(), 1);

    let mut a2 = h.connect().await;
    identify(&mut a2, &window_a).await;
    let leftover = expect_reports(call_wait_for_report(&mut a2, 0).await);
    assert!(
        leftover.is_empty(),
        "a report handed directly to a waiter must not also sit in the inbox"
    );
}
