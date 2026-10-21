#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the plan review gate at the daemon's IPC boundary --
//! a plan created while `plan_approval_required` is on waits in `pending`,
//! its owner cannot open task windows until an approver settles it with
//! `approve_plan`, the owner is told once through a one-line
//! `[yagura plan]` notice, and tasks added or changed after approval carry a
//! review mark until an approver sends `mark_plan_seen`.
//!
//! Every case sets `plan_approval_required` explicitly rather than relying on
//! the built-in default. Windows are `echo_child`, `ready_child` or a
//! composer-draw `submit_child`, never a real vendor CLI, never a running
//! daemon.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::{
    Harness, SubmitDefinitionInput, approver_base_config, call_list_windows, call_open_window,
    detection_config, expect_error, identify, open_submit_child_window_with_composer,
    read_response, register_approver, send, text_tail, unique_temp_dir, wait_until_status,
    write_profile,
};

use yagura_daemon::DaemonConfig;
use yagura_protocol::{
    AssignmentSource, ClientMessage, DaemonMessage, PlanInfo, PlanReview, PlanSummary,
    PlanTaskInfo, PlanTaskSpec, ReviewMark, TaskAssignment, WindowStatus,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{echo_child_command, ready_child_profile_source};

/// The fixed marker every plan notice starts with.
const PLAN_NOTICE_MARKER: &str = "[yagura plan]";

/// Upper bound for a reply or a notice that is expected to arrive.
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a case watches a ready owner for a notice that must not be typed
/// again. A notice that is going to be typed lands after the ready hold plus
/// the submit delay (a quarter of a second here), so this is more than ten
/// times that.
const ABSENCE_WINDOW: Duration = Duration::from_secs(3);

/// The owner's `[input] ready_hold_ms`.
const READY_HOLD_MS: u64 = 200;

const OWNER_MARKER: &str = "prg-owner-ready";

const TASK_PROFILE: &str = "prg-task-profile";
const OVERRIDE_PROFILE: &str = "prg-override-profile";

/// The notice text, built from the fixed wording rather than taken from the
/// product, so the product cannot move the expectation along with itself.
fn expected_notice(plan_id: &str) -> String {
    format!("{PLAN_NOTICE_MARKER} Plan {plan_id} was approved. You can open its tasks now.")
}

// --- config ------------------------------------------------------------------

struct Dirs {
    root: PathBuf,
    profiles: PathBuf,
}

fn dirs(tag: &str) -> Dirs {
    let root = unique_temp_dir(tag);
    let profiles = root.join("profiles");
    std::fs::create_dir_all(&profiles).expect("create profiles dir");
    write_profile(
        &profiles,
        TASK_PROFILE,
        &ready_child_profile_source("PRG task"),
    );
    write_profile(
        &profiles,
        OVERRIDE_PROFILE,
        &ready_child_profile_source("PRG override"),
    );
    Dirs { root, profiles }
}

/// A ledger-backed config with `plan_approval_required` set to `required`,
/// and `close_window` auto-approved so a case can kill an owner.
fn gate_config(dirs: &Dirs, required: bool) -> DaemonConfig {
    let mut config = detection_config(&dirs.root, approver_base_config());
    config.approval_policy = Some(common::policy_opening(&["close_window"]));
    config.store_db_path_resolved = Some(dirs.root.join("plan.db"));
    config.profiles_dir = Some(dirs.profiles.clone());
    config.plan_approval_required = required;
    config
}

async fn start(tag: &str, required: bool) -> (Harness, Dirs) {
    let dirs = dirs(tag);
    let (h, _locks) = Harness::start_with_ledger(tag, gate_config(&dirs, required)).await;
    (h, dirs)
}

// --- task builders -----------------------------------------------------------

fn task(task_id: &str, title: &str) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: task_id.to_string(),
        title: title.to_string(),
        role: None,
        profile: Some(TASK_PROFILE.to_string()),
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: None,
    }
}

// --- connections -------------------------------------------------------------

/// Opens a plain `echo_child` window and identifies `client` as it, so the
/// connection can own a plan.
async fn open_plain_owner(h: &Harness, client: &mut Connection) -> String {
    let window_id = h.open(echo_child_command(), None).await;
    identify(client, &window_id).await;
    window_id
}

async fn approver_connection(h: &Harness) -> Connection {
    let mut client = h.connect().await;
    register_approver(&mut client).await;
    client
}

fn owner_input() -> SubmitDefinitionInput {
    SubmitDefinitionInput {
        bracketed_paste: false,
        ready_hold_ms: READY_HOLD_MS,
        submit_delay_ms: 50,
        clear: "",
        delivery_confirm_ms: 0,
        resend_max: 0,
    }
}

/// An owner that can be typed into: a composer-draw `submit_child` that
/// prints `SUBMITTED <line>` for every line written into it, ready again after
/// `common::redraw_screen`, and a connection identified as it.
struct ReadyOwner {
    window_id: String,
    conn: Connection,
}

async fn open_ready_owner(h: &Harness, dirs: &Dirs, probe: &mut Connection) -> ReadyOwner {
    let window_id = open_submit_child_window_with_composer(
        h,
        probe,
        &dirs.root,
        "prg-owner",
        &common::ready_pattern_with_redraw(OWNER_MARKER),
        0,
        0,
        0,
        owner_input(),
        0,
    )
    .await;
    let mut conn = h.connect().await;
    identify(&mut conn, &window_id).await;
    ReadyOwner { window_id, conn }
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
    match read_ledger_reply(closer, Duration::from_secs(10)).await {
        DaemonMessage::WindowClosed { window_id: closed } => assert_eq!(closed, window_id),
        other => panic!("expected window_closed, got {other:?}"),
    }
}

// --- wire round trips --------------------------------------------------------

/// Reads the next *response* frame, skipping `plan_changed` pushes: a
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

fn expect_plan_detail(msg: DaemonMessage) -> PlanInfo {
    match msg {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

async fn request(client: &mut Connection, msg: ClientMessage) -> DaemonMessage {
    send(client, &msg).await;
    read_ledger_reply(client, REPLY_TIMEOUT).await
}

async fn create_plan(client: &mut Connection, title: &str, tasks: Vec<PlanTaskSpec>) -> PlanInfo {
    expect_plan_detail(
        request(
            client,
            ClientMessage::CreatePlan {
                title: title.to_string(),
                tasks,
            },
        )
        .await,
    )
}

async fn update_plan(client: &mut Connection, plan_id: &str, tasks: Vec<PlanTaskSpec>) -> PlanInfo {
    expect_plan_detail(
        request(
            client,
            ClientMessage::UpdatePlan {
                plan_id: plan_id.to_string(),
                title: None,
                tasks,
            },
        )
        .await,
    )
}

async fn get_plan(client: &mut Connection, plan_id: &str) -> PlanInfo {
    expect_plan_detail(
        request(
            client,
            ClientMessage::GetPlan {
                plan_id: plan_id.to_string(),
            },
        )
        .await,
    )
}

async fn plan_summary(client: &mut Connection, plan_id: &str) -> PlanSummary {
    match request(client, ClientMessage::ListPlans).await {
        DaemonMessage::PlanList { plans } => plans
            .into_iter()
            .find(|p| p.plan_id == plan_id)
            .unwrap_or_else(|| panic!("plan {plan_id} must be listed")),
        other => panic!("expected plan_list, got {other:?}"),
    }
}

async fn approve_plan(client: &mut Connection, plan_id: &str, version: u64) -> DaemonMessage {
    request(
        client,
        ClientMessage::ApprovePlan {
            plan_id: plan_id.to_string(),
            version,
        },
    )
    .await
}

async fn mark_plan_seen(client: &mut Connection, plan_id: &str, version: u64) -> DaemonMessage {
    request(
        client,
        ClientMessage::MarkPlanSeen {
            plan_id: plan_id.to_string(),
            version,
        },
    )
    .await
}

async fn approve_ok(approver: &mut Connection, plan_id: &str, version: u64) -> PlanInfo {
    let plan = expect_plan_detail(approve_plan(approver, plan_id, version).await);
    assert_eq!(
        plan.review,
        Some(PlanReview::Approved),
        "approve_plan must answer with the plan settled to approved"
    );
    plan
}

async fn open_task_window(client: &mut Connection, plan_id: &str, task_id: &str) -> DaemonMessage {
    request(
        client,
        ClientMessage::OpenTaskWindow {
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
            name: None,
        },
    )
    .await
}

async fn set_override(approver: &mut Connection, plan_id: &str, task_id: &str, profile: &str) {
    expect_plan_detail(
        request(
            approver,
            ClientMessage::SetTaskAssignment {
                plan_id: plan_id.to_string(),
                task_id: task_id.to_string(),
                assignment: TaskAssignment {
                    profile: profile.to_string(),
                    model: None,
                    effort: None,
                },
            },
        )
        .await,
    );
}

/// Reads frames on a freshly registered approver until a `plan_changed` for
/// `plan_id` arrives. Registered only right before the change under test,
/// so no earlier change can satisfy it.
async fn expect_plan_changed(observer: &mut Connection, plan_id: &str) {
    let deadline = tokio::time::Instant::now() + REPLY_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for plan_changed for {plan_id}"
        );
        if let DaemonMessage::PlanChanged { plan_id: changed } =
            read_response(observer, remaining).await
            && changed == plan_id
        {
            return;
        }
    }
}

fn task_info<'a>(plan: &'a PlanInfo, task_id: &str) -> &'a PlanTaskInfo {
    plan.tasks
        .iter()
        .find(|t| t.task_id == task_id)
        .unwrap_or_else(|| panic!("task {task_id} not found in plan {plan:?}"))
}

fn marks(plan: &PlanInfo) -> Vec<(String, Option<ReviewMark>)> {
    let mut marks: Vec<_> = plan
        .tasks
        .iter()
        .map(|t| (t.task_id.clone(), t.review_mark))
        .collect();
    marks.sort_by(|a, b| a.0.cmp(&b.0));
    marks
}

// --- reading what reached the owner's input ------------------------------------

async fn wait_for_submitted(h: &Harness, window_id: &str, text: &str) {
    let deadline = tokio::time::Instant::now() + REPLY_TIMEOUT;
    loop {
        if h.submitted_count(window_id, text) >= 1 {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for {window_id} to accept {text:?}; flattened scrollback \
                 tail: {:?}",
                text_tail(&h.flattened_scrollback(window_id)),
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Watches the window for [`ABSENCE_WINDOW`], failing as soon as it has
/// accepted `text` more than `allowed` times.
async fn assert_submitted_stays_at(h: &Harness, window_id: &str, text: &str, allowed: usize) {
    let deadline = tokio::time::Instant::now() + ABSENCE_WINDOW;
    loop {
        let seen = h.submitted_count(window_id, text);
        assert!(
            seen <= allowed,
            "the window accepted {text:?} {seen} time(s), expected {allowed}; flattened \
             scrollback tail: {:?}",
            text_tail(&h.flattened_scrollback(window_id)),
        );
        if tokio::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// --- creation --------------------------------------------------------------------

#[tokio::test]
async fn a_plan_created_while_approval_is_required_starts_pending_with_a_version() {
    let (h, _dirs) = start("prg-create-pending", true).await;
    let mut owner = h.connect().await;
    open_plain_owner(&h, &mut owner).await;

    let created = create_plan(&mut owner, "plan", vec![task("t1", "first")]).await;
    assert_eq!(created.review, Some(PlanReview::Pending));
    assert_eq!(created.version, 0);

    let read = get_plan(&mut owner, &created.plan_id).await;
    assert_eq!(read.review, Some(PlanReview::Pending));
    assert_eq!(read.version, 0);

    let mut approver = approver_connection(&h).await;
    let summary = plan_summary(&mut approver, &created.plan_id).await;
    assert_eq!(summary.review, Some(PlanReview::Pending));
    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_plan_created_while_approval_is_not_required_carries_no_review() {
    let (h, _dirs) = start("prg-create-unreviewed", false).await;
    let mut owner = h.connect().await;
    open_plain_owner(&h, &mut owner).await;

    let created = create_plan(&mut owner, "plan", vec![task("t1", "first")]).await;
    assert_eq!(created.review, None);
    assert_eq!(get_plan(&mut owner, &created.plan_id).await.review, None);
    h.registry.terminate_all().await;
}

// --- the gate on open_task_window ---------------------------------------------------

#[tokio::test]
async fn a_pending_plan_refuses_its_owners_open_task_window_and_opens_nothing() {
    let (h, _dirs) = start("prg-pending-refuses", true).await;
    let mut owner = h.connect().await;
    open_plain_owner(&h, &mut owner).await;
    let plan = create_plan(&mut owner, "plan", vec![task("t1", "first")]).await;
    let mut lister = h.connect().await;
    let before = call_list_windows(&mut lister).await.len();

    let reply = open_task_window(&mut owner, &plan.plan_id, "t1").await;
    expect_error(reply, "plan_not_approved");

    let after = call_list_windows(&mut lister).await.len();
    assert_eq!(
        after, before,
        "a refused open_task_window must not open a window"
    );
    let read = get_plan(&mut owner, &plan.plan_id).await;
    assert_eq!(task_info(&read, "t1").window_id, None);
    h.registry.terminate_all().await;
}

#[tokio::test]
async fn approving_a_plan_pushes_plan_changed_and_lets_the_owner_open_with_the_users_override() {
    let (h, _dirs) = start("prg-approve-opens", true).await;
    let mut owner = h.connect().await;
    open_plain_owner(&h, &mut owner).await;
    let plan = create_plan(&mut owner, "plan", vec![task("t1", "first")]).await;
    let mut approver = approver_connection(&h).await;
    set_override(&mut approver, &plan.plan_id, "t1", OVERRIDE_PROFILE).await;
    let version = get_plan(&mut owner, &plan.plan_id).await.version;

    let mut observer = approver_connection(&h).await;
    approve_ok(&mut approver, &plan.plan_id, version).await;
    expect_plan_changed(&mut observer, &plan.plan_id).await;
    assert_eq!(
        get_plan(&mut owner, &plan.plan_id).await.review,
        Some(PlanReview::Approved)
    );

    match open_task_window(&mut owner, &plan.plan_id, "t1").await {
        DaemonMessage::TaskWindowOpened { assignment, .. } => {
            assert_eq!(assignment.source, AssignmentSource::User);
            assert_eq!(
                assignment.assignment.map(|a| a.profile),
                Some(OVERRIDE_PROFILE.to_string())
            );
        }
        other => panic!("expected task_window_opened, got {other:?}"),
    }
    h.registry.terminate_all().await;
}

// --- approve_plan's refusals and idempotence ------------------------------------------

#[tokio::test]
async fn approving_with_a_stale_version_is_invalid_state_and_leaves_the_plan_pending() {
    let (h, _dirs) = start("prg-stale-version", true).await;
    let mut owner = h.connect().await;
    open_plain_owner(&h, &mut owner).await;
    let plan = create_plan(&mut owner, "plan", vec![task("t1", "first")]).await;
    let stale = plan.version;
    let updated = update_plan(
        &mut owner,
        &plan.plan_id,
        vec![task("t1", "first"), task("t2", "second")],
    )
    .await;
    assert_ne!(
        updated.version, stale,
        "replacing tasks must move the version"
    );
    let mut approver = approver_connection(&h).await;

    expect_error(
        approve_plan(&mut approver, &plan.plan_id, stale).await,
        "invalid_state",
    );
    assert_eq!(
        get_plan(&mut owner, &plan.plan_id).await.review,
        Some(PlanReview::Pending)
    );

    approve_ok(&mut approver, &plan.plan_id, updated.version).await;
    h.registry.terminate_all().await;
}

#[tokio::test]
async fn approving_a_plan_that_needs_no_review_is_invalid_state() {
    let (h, _dirs) = start("prg-approve-unreviewed", false).await;
    let mut owner = h.connect().await;
    open_plain_owner(&h, &mut owner).await;
    let plan = create_plan(&mut owner, "plan", vec![task("t1", "first")]).await;
    let mut approver = approver_connection(&h).await;

    expect_error(
        approve_plan(&mut approver, &plan.plan_id, plan.version).await,
        "invalid_state",
    );
    assert_eq!(get_plan(&mut owner, &plan.plan_id).await.review, None);
    h.registry.terminate_all().await;
}

#[tokio::test]
async fn approving_an_approved_plan_again_succeeds_unchanged_and_does_not_notify_again() {
    let (h, dirs) = start("prg-reapprove", true).await;
    let mut probe = h.connect().await;
    let mut owner = open_ready_owner(&h, &dirs, &mut probe).await;
    let plan = create_plan(&mut owner.conn, "plan", vec![task("t1", "first")]).await;
    let mut approver = approver_connection(&h).await;
    let notice = expected_notice(&plan.plan_id);

    let first = approve_ok(&mut approver, &plan.plan_id, plan.version).await;
    wait_for_submitted(&h, &owner.window_id, &notice).await;

    let again = approve_ok(&mut approver, &plan.plan_id, plan.version).await;
    assert_eq!(again.version, first.version);
    assert_eq!(again.tasks, first.tasks);

    assert_submitted_stays_at(&h, &owner.window_id, &notice, 1).await;

    // A later notice queued behind any re-sent one would land after it. The
    // typed notice left the owner's screen running; it takes the next one
    // once it has redrawn its prompt.
    common::redraw_screen(
        &h,
        &owner.window_id,
        &common::ready_pattern_with_redraw(OWNER_MARKER),
    )
    .await;
    wait_until_status(
        &mut probe,
        &owner.window_id,
        WindowStatus::WaitingInput,
        Duration::from_secs(10),
    )
    .await;
    let sentinel = create_plan(&mut owner.conn, "sentinel", vec![task("t1", "first")]).await;
    approve_ok(&mut approver, &sentinel.plan_id, sentinel.version).await;
    wait_for_submitted(&h, &owner.window_id, &expected_notice(&sentinel.plan_id)).await;
    assert_eq!(
        h.submitted_count(&owner.window_id, &notice),
        1,
        "approving an approved plan again must not type its notice again"
    );
    h.registry.terminate_all().await;
}

#[tokio::test]
async fn the_owners_own_window_cannot_approve_or_mark_its_plan_seen() {
    let (h, _dirs) = start("prg-owner-not-approver", true).await;
    let mut owner = h.connect().await;
    open_plain_owner(&h, &mut owner).await;
    let plan = create_plan(&mut owner, "plan", vec![task("t1", "first")]).await;

    expect_error(
        approve_plan(&mut owner, &plan.plan_id, plan.version).await,
        "auth_failed",
    );
    expect_error(
        mark_plan_seen(&mut owner, &plan.plan_id, plan.version).await,
        "auth_failed",
    );
    assert_eq!(
        get_plan(&mut owner, &plan.plan_id).await.review,
        Some(PlanReview::Pending)
    );
    h.registry.terminate_all().await;
}

// --- the approval notice ----------------------------------------------------------------

#[tokio::test]
async fn approving_types_the_fixed_notice_into_the_owner_without_the_title() {
    let (h, dirs) = start("prg-notice-text", true).await;
    let mut probe = h.connect().await;
    let mut owner = open_ready_owner(&h, &dirs, &mut probe).await;
    let title = "PRGTITLEMUSTNOTAPPEAR";
    let plan = create_plan(&mut owner.conn, title, vec![task("t1", "first")]).await;
    let mut approver = approver_connection(&h).await;

    approve_ok(&mut approver, &plan.plan_id, plan.version).await;
    wait_for_submitted(&h, &owner.window_id, &expected_notice(&plan.plan_id)).await;

    let flattened = h.flattened_scrollback(&owner.window_id);
    assert!(
        !flattened.contains(title),
        "the notice must carry the plan id only, never its title: {:?}",
        text_tail(&flattened),
    );
    h.registry.terminate_all().await;
}

#[tokio::test]
async fn approving_two_plans_of_one_owner_back_to_back_types_both_notices() {
    let (h, dirs) = start("prg-notice-two", true).await;
    let mut probe = h.connect().await;
    let mut owner = open_ready_owner(&h, &dirs, &mut probe).await;
    let first = create_plan(&mut owner.conn, "first", vec![task("t1", "first")]).await;
    let second = create_plan(&mut owner.conn, "second", vec![task("t1", "first")]).await;
    let mut approver = approver_connection(&h).await;

    approve_ok(&mut approver, &first.plan_id, first.version).await;
    approve_ok(&mut approver, &second.plan_id, second.version).await;

    wait_for_submitted(&h, &owner.window_id, &expected_notice(&first.plan_id)).await;
    // The typed notice left the owner's screen running; the second notice
    // waits for the prompt to be redrawn, as it would behind a real CLI.
    common::redraw_screen(
        &h,
        &owner.window_id,
        &common::ready_pattern_with_redraw(OWNER_MARKER),
    )
    .await;
    wait_for_submitted(&h, &owner.window_id, &expected_notice(&second.plan_id)).await;
    h.registry.terminate_all().await;
}

#[tokio::test]
async fn approving_a_plan_whose_owner_died_types_nothing_into_another_live_window() {
    let (h, dirs) = start("prg-notice-dead-owner", true).await;
    let mut probe = h.connect().await;
    // The opener is a ready window the dead owner's notice could wrongly
    // fall back to.
    let mut opener = open_ready_owner(&h, &dirs, &mut probe).await;
    let dead_owner = call_open_window(&mut opener.conn, echo_child_command()).await;
    let mut dead_conn = h.connect().await;
    identify(&mut dead_conn, &dead_owner).await;
    let orphaned = create_plan(&mut dead_conn, "orphaned", vec![task("t1", "first")]).await;

    let mut approver = approver_connection(&h).await;
    close_window(&mut approver, &dead_owner).await;

    approve_ok(&mut approver, &orphaned.plan_id, orphaned.version).await;

    // The opener's own plan is approved afterwards: its notice is queued
    // behind anything the orphaned plan's approval queued on the opener.
    let sentinel = create_plan(&mut opener.conn, "sentinel", vec![task("t1", "first")]).await;
    approve_ok(&mut approver, &sentinel.plan_id, sentinel.version).await;
    wait_for_submitted(&h, &opener.window_id, &expected_notice(&sentinel.plan_id)).await;

    let flattened = h.flattened_scrollback(&opener.window_id);
    assert!(
        !flattened.contains(&format!("Plan {}", orphaned.plan_id)),
        "a plan whose owner is dead must not be announced to another window: {:?}",
        text_tail(&flattened),
    );
    assert_eq!(
        h.submitted_count(&opener.window_id, PLAN_NOTICE_MARKER),
        1,
        "only the opener's own plan may be announced to it"
    );
    h.registry.terminate_all().await;
}

// --- review marks after approval ------------------------------------------------------------

#[tokio::test]
async fn tasks_added_or_changed_after_approval_open_and_carry_marks_until_marked_seen() {
    let (h, _dirs) = start("prg-review-marks", true).await;
    let mut owner = h.connect().await;
    open_plain_owner(&h, &mut owner).await;
    let plan = create_plan(
        &mut owner,
        "plan",
        vec![task("t1", "first"), task("t3", "third")],
    )
    .await;
    let mut approver = approver_connection(&h).await;
    let approved = approve_ok(&mut approver, &plan.plan_id, plan.version).await;
    let approved_version = approved.version;

    let updated = update_plan(
        &mut owner,
        &plan.plan_id,
        vec![
            task("t1", "first, reworded"),
            task("t2", "second"),
            task("t3", "third"),
        ],
    )
    .await;
    assert_eq!(updated.review, Some(PlanReview::Approved));
    let expected_marks = vec![
        ("t1".to_string(), Some(ReviewMark::Changed)),
        ("t2".to_string(), Some(ReviewMark::New)),
        ("t3".to_string(), None),
    ];
    assert_eq!(
        marks(&get_plan(&mut owner, &plan.plan_id).await),
        expected_marks
    );
    assert!(
        plan_summary(&mut approver, &plan.plan_id)
            .await
            .unseen_changes
    );

    for task_id in ["t1", "t2"] {
        match open_task_window(&mut owner, &plan.plan_id, task_id).await {
            DaemonMessage::TaskWindowOpened { .. } => {}
            other => panic!("expected task_window_opened for {task_id}, got {other:?}"),
        }
    }

    let stale =
        expect_plan_detail(mark_plan_seen(&mut approver, &plan.plan_id, approved_version).await);
    assert_eq!(
        marks(&stale),
        expected_marks,
        "a stale mark_plan_seen must answer with the marks still in place"
    );
    assert_eq!(
        marks(&get_plan(&mut owner, &plan.plan_id).await),
        expected_marks
    );

    let current = get_plan(&mut owner, &plan.plan_id).await.version;
    let mut observer = approver_connection(&h).await;
    let cleared = expect_plan_detail(mark_plan_seen(&mut approver, &plan.plan_id, current).await);
    expect_plan_changed(&mut observer, &plan.plan_id).await;
    assert!(
        cleared.tasks.iter().all(|t| t.review_mark.is_none()),
        "mark_plan_seen at the current version must clear every mark: {cleared:?}"
    );
    assert!(
        !plan_summary(&mut approver, &plan.plan_id)
            .await
            .unseen_changes
    );
    h.registry.terminate_all().await;
}
