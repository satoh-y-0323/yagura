#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Seeds and reads the database with raw SQL, which only tests may do.
#![allow(clippy::disallowed_methods)]

//! Integration test: the plan ledger's remaining IPC surface -- the user
//! assignment override (`set_task_assignment` / `clear_task_assignment`),
//! owner nomination (`set_plan_owner`), the role table (`list_roles` /
//! `save_roles`) and `open_task_window` (including its serialization and its
//! failure-cleanup path) -- driven through `Harness::start_with_ledger`.
//!
//! `create_plan` / `update_plan` / `get_plan` / `list_plans` / `delete_plan`
//! authorization is covered by `ipc_plan.rs`; this file only re-exercises
//! them where a case needs a bound task window or a role-resolved task as a
//! precondition. Children are `argv_probe`, `ready_child` or a fixed
//! self-exiting command via a launch profile, never a real vendor CLI.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{
    Harness, approver_base_config, expect_error, identify, read_response, register_approver, send,
    unique_temp_dir, write_profile,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::plan_permit::LockTable;
use yagura_protocol::{
    ClientMessage, DaemonMessage, PlanInfo, PlanTaskSpec, ResolvedAssignment, RoleEntry,
    TaskAssignment,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{
    argv_probe_profile_source, exit_with_code_command, ready_child_profile_source,
};

// --- fixture profiles ----------------------------------------------------

/// A profile launching `argv_probe`, mapping both axes with distinct flags so
/// a test can tell which of two such profiles a launch actually used.
fn write_argv_profile(dir: &Path, id: &str, display_name: &str, model_flag: &str) {
    let tables = format!(
        "\n[model]\nargs = [\"{model_flag}\", \"{{value}}\"]\n\n[effort]\nargs = [\"-c\", \
         \"tier={{value}}\"]\n"
    );
    write_profile(dir, id, &argv_probe_profile_source(display_name, &tables));
}

/// A profile with no launch-option tables at all: usable everywhere a case
/// only needs a valid, existing profile id and never opens the window (an
/// assignment-override target) or opens one whose argv nobody inspects.
fn write_plain_profile(dir: &Path, id: &str, display_name: &str) {
    write_profile(dir, id, &ready_child_profile_source(display_name));
}

/// A profile whose `[command]` is a fixed self-exiting process: the profile
/// counterpart of `exit_with_code_command`, for a task window that must exit
/// on its own without a human closing it.
fn write_self_exiting_profile(dir: &Path, id: &str, display_name: &str) {
    let argv = exit_with_code_command("0");
    let elements: Vec<String> = argv.iter().map(|element| format!("'{element}'")).collect();
    let source = format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\n\n[command]\ndefault = [{}]\n",
        elements.join(", ")
    );
    write_profile(dir, id, &source);
}

// --- config -----------------------------------------------------------

/// A ledger-backed config under `dir`, with `profiles_dir` and `roles_path`
/// resolved the way `main.rs` is expected to resolve them, and
/// `auto_approve` set to whatever destructive tools (`close_window`, most
/// commonly) a case needs to drive without a human decision.
///
/// `plan_approval_required` is off: this file measures assignment
/// resolution and `open_task_window`'s own binding logic, not the review
/// gate (`plan_review_gate.rs`'s job), so a plan created here must not start
/// pending.
fn assign_config(
    dir: &Path,
    profiles_dir: PathBuf,
    roles_path: Option<PathBuf>,
    auto_approve: &[&str],
) -> DaemonConfig {
    let mut config = approver_base_config();
    config.approval_policy = Some(common::policy_opening(auto_approve));
    config.store_db_path_resolved = Some(dir.join("plan.db"));
    config.profiles_dir = Some(profiles_dir);
    config.roles_path = roles_path;
    config.plan_approval_required = false;
    config
}

async fn start_ledger(tag: &str, config: DaemonConfig) -> (Harness, Arc<LockTable>) {
    Harness::start_with_ledger(tag, config).await
}

// --- task builders ---------------------------------------------------------

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

fn task_with_profile_options(
    task_id: &str,
    profile: &str,
    model: &str,
    effort: &str,
) -> PlanTaskSpec {
    PlanTaskSpec {
        profile: Some(profile.to_string()),
        model: Some(model.to_string()),
        effort: Some(effort.to_string()),
        ..task(task_id)
    }
}

fn task_with_role(task_id: &str, role: &str) -> PlanTaskSpec {
    PlanTaskSpec {
        role: Some(role.to_string()),
        ..task(task_id)
    }
}

// --- connection helpers ------------------------------------------------

async fn open_and_identify(h: &Harness, client: &mut Connection) -> String {
    let window_id = h
        .open(yagura_test_support::process::echo_child_command(), None)
        .await;
    identify(client, &window_id).await;
    window_id
}

async fn approver_connection(h: &Harness) -> Connection {
    let mut client = h.connect().await;
    register_approver(&mut client).await;
    client
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

/// Reads the next *response* frame, skipping `plan_changed` pushes the same
/// way `ipc_plan.rs`'s own helper does: a connection that is also a
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

/// Asserts no frame (a reply or a push, `plan_changed` included) arrives on
/// `client` within `wait`. Reads the raw next frame via `read_response`
/// rather than `read_ledger_reply`: the latter's own `plan_changed`-skipping
/// loop would keep waiting past a `plan_changed` push instead of surfacing
/// it, which would make this assertion pass even when one arrived. The
/// inner read is given a much longer timeout than `wait` so the outer race
/// is decided by `wait`, not by the inner call's own panic-on-timeout.
async fn expect_no_message_within(client: &mut Connection, wait: Duration) {
    let outcome = tokio::time::timeout(wait, read_response(client, Duration::from_secs(30))).await;
    assert!(
        outcome.is_err(),
        "expected no message to arrive within {wait:?}, but got {:?}",
        outcome.ok()
    );
}

// --- wire round trips ----------------------------------------------------

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

async fn list_plans(client: &mut Connection) -> DaemonMessage {
    send(client, &ClientMessage::ListPlans).await;
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

async fn set_task_assignment(
    client: &mut Connection,
    plan_id: &str,
    task_id: &str,
    assignment: TaskAssignment,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::SetTaskAssignment {
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
            assignment,
        },
    )
    .await;
    read_ledger_reply(client, Duration::from_secs(5)).await
}

async fn clear_task_assignment(
    client: &mut Connection,
    plan_id: &str,
    task_id: &str,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::ClearTaskAssignment {
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
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

async fn list_roles(client: &mut Connection) -> DaemonMessage {
    send(client, &ClientMessage::ListRoles).await;
    read_ledger_reply(client, Duration::from_secs(5)).await
}

async fn save_roles(client: &mut Connection, roles: Vec<RoleEntry>) -> DaemonMessage {
    send(
        client,
        &ClientMessage::SaveRoles {
            roles,
            default_role: None,
        },
    )
    .await;
    read_ledger_reply(client, Duration::from_secs(5)).await
}

async fn open_task_window(
    client: &mut Connection,
    plan_id: &str,
    task_id: &str,
    name: Option<&str>,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::OpenTaskWindow {
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
            name: name.map(str::to_string),
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

fn expect_role_list(msg: DaemonMessage) -> Vec<RoleEntry> {
    match msg {
        DaemonMessage::RoleList { roles, .. } => roles,
        other => panic!("expected role_list, got {other:?}"),
    }
}

#[allow(clippy::type_complexity)]
fn expect_task_window_opened(msg: DaemonMessage) -> (String, String, String, ResolvedAssignment) {
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

// --- assignment override: approver-only, and how it resolves -------------

#[tokio::test]
async fn a_window_is_always_refused_for_assignment_overrides_even_for_a_missing_plan() {
    let dir = unique_temp_dir("plan-assign-override-scope");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "target-profile", "Target");
    let (h, _locks) = start_ledger(
        "plan-assign-override-scope",
        assign_config(&dir, profiles_dir, None, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(&mut owner, "plan", vec![task("t1")]).await;

    let assignment = TaskAssignment {
        profile: "target-profile".to_string(),
        model: None,
        effort: None,
    };
    let reply = set_task_assignment(&mut owner, &plan.plan_id, "t1", assignment.clone()).await;
    expect_error(reply, "auth_failed");
    let reply = clear_task_assignment(&mut owner, &plan.plan_id, "t1").await;
    expect_error(reply, "auth_failed");

    // Even a plan id nobody ever created is refused the same way: origin is
    // decided before the plan is ever looked up.
    let reply = set_task_assignment(&mut owner, "no-such-plan", "t1", assignment).await;
    expect_error(reply, "auth_failed");
}

#[tokio::test]
async fn an_approver_override_wins_and_survives_an_update_that_keeps_the_task_id() {
    let dir = unique_temp_dir("plan-assign-override-resolution");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "override-profile", "Override");
    let (h, _locks) = start_ledger(
        "plan-assign-override-resolution",
        assign_config(&dir, profiles_dir, None, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(&mut owner, "plan", vec![task("t1"), task("t2")]).await;

    let mut approver = approver_connection(&h).await;
    let assignment = TaskAssignment {
        profile: "override-profile".to_string(),
        model: None,
        effort: None,
    };
    let updated = expect_plan_detail(
        set_task_assignment(&mut approver, &plan.plan_id, "t1", assignment.clone()).await,
    );
    let t1 = task_info(&updated, "t1");
    assert_eq!(t1.user.as_ref(), Some(&assignment));
    assert_eq!(t1.resolved.source, yagura_protocol::AssignmentSource::User);
    assert_eq!(t1.resolved.assignment.as_ref(), Some(&assignment));

    // A replacement that keeps "t1" carries the override over; "t2" is
    // dropped from the replacement, so its slot (never overridden) proves
    // nothing either way, but "t1" must still show the same override.
    let replaced =
        expect_plan_detail(update_plan(&mut owner, &plan.plan_id, vec![task("t1")]).await);
    let t1_after = task_info(&replaced, "t1");
    assert_eq!(t1_after.user.as_ref(), Some(&assignment));

    let cleared =
        expect_plan_detail(clear_task_assignment(&mut approver, &plan.plan_id, "t1").await);
    let t1_cleared = task_info(&cleared, "t1");
    assert_eq!(t1_cleared.user, None);
    assert_eq!(
        t1_cleared.resolved.source,
        yagura_protocol::AssignmentSource::Unassigned
    );
}

#[tokio::test]
async fn replacing_a_task_id_drops_its_override() {
    let dir = unique_temp_dir("plan-assign-override-replace-drops");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "override-profile", "Override");
    let (h, _locks) = start_ledger(
        "plan-assign-override-replace-drops",
        assign_config(&dir, profiles_dir, None, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(&mut owner, "plan", vec![task("t1")]).await;

    let mut approver = approver_connection(&h).await;
    let assignment = TaskAssignment {
        profile: "override-profile".to_string(),
        model: None,
        effort: None,
    };
    expect_plan_detail(set_task_assignment(&mut approver, &plan.plan_id, "t1", assignment).await);

    // "t1" is dropped from the replacement entirely, so nothing carries its
    // override over; the replacement's own "t1" (re-registered fresh) must
    // start with no override.
    let replaced =
        expect_plan_detail(update_plan(&mut owner, &plan.plan_id, vec![task("t2")]).await);
    assert!(
        replaced
            .tasks
            .iter()
            .all(|t| t.task_id != "t1" || t.user.is_none())
    );
    let replaced_again =
        expect_plan_detail(update_plan(&mut owner, &plan.plan_id, vec![task("t1")]).await);
    let t1 = task_info(&replaced_again, "t1");
    assert_eq!(
        t1.user, None,
        "a task_id re-registered after being dropped must not inherit a stale override"
    );
}

// --- assignment override: validation ------------------------------------

#[tokio::test]
async fn an_override_is_validated_before_it_is_written() {
    let dir = unique_temp_dir("plan-assign-override-validation");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "real-profile", "Real");
    let (h, _locks) = start_ledger(
        "plan-assign-override-validation",
        assign_config(&dir, profiles_dir, None, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(&mut owner, "plan", vec![task("t1")]).await;
    let mut approver = approver_connection(&h).await;

    let reply = set_task_assignment(
        &mut approver,
        &plan.plan_id,
        "t1",
        TaskAssignment {
            profile: "profile-does-not-exist".to_string(),
            model: None,
            effort: None,
        },
    )
    .await;
    expect_error(reply, "invalid_argument");

    let reply = set_task_assignment(
        &mut approver,
        &plan.plan_id,
        "t1",
        TaskAssignment {
            profile: "real-profile".to_string(),
            model: Some("poison-\u{7}-value".to_string()),
            effort: None,
        },
    )
    .await;
    let message = expect_error(reply, "invalid_argument");
    assert!(
        !message.contains("poison-"),
        "a rejected value must never be echoed back, got {message:?}"
    );

    let unaffected = expect_plan_detail(get_plan(&mut owner, &plan.plan_id).await);
    assert_eq!(
        task_info(&unaffected, "t1").user,
        None,
        "a rejected override must never be written"
    );
}

// --- owner nomination ---------------------------------------------------

#[tokio::test]
async fn nominating_a_new_owner_replaces_the_old_one() {
    let dir = unique_temp_dir("plan-assign-nominate-replace");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    let (h, _locks) = start_ledger(
        "plan-assign-nominate-replace",
        assign_config(&dir, profiles_dir, None, &[]),
    )
    .await;

    let mut owner_a = h.connect().await;
    let window_a = open_and_identify(&h, &mut owner_a).await;
    let plan = create_ok(&mut owner_a, "plan", vec![task("t1")]).await;

    let mut owner_b = h.connect().await;
    let window_b = open_and_identify(&h, &mut owner_b).await;

    let mut approver = approver_connection(&h).await;
    let nominated =
        expect_plan_detail(set_plan_owner(&mut approver, &plan.plan_id, &window_b).await);
    assert_eq!(nominated.owner_window.as_deref(), Some(window_b.as_str()));

    // The new owner can now write the plan's structure.
    expect_plan_detail(
        update_plan(&mut owner_b, &plan.plan_id, vec![task("t1"), task("t2")]).await,
    );

    // The old owner (never bound to any task) is now out of range.
    let reply = update_plan(&mut owner_a, &plan.plan_id, vec![task("t1")]).await;
    expect_error(reply, "plan_not_found");
    let _ = window_a;
}

#[tokio::test]
async fn nominating_checks_the_targets_registration_and_liveness() {
    let dir = unique_temp_dir("plan-assign-nominate-target-state");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    let (h, _locks) = start_ledger(
        "plan-assign-nominate-target-state",
        assign_config(&dir, profiles_dir, None, &["close_window"]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(&mut owner, "plan", vec![task("t1")]).await;
    let mut approver = approver_connection(&h).await;

    // Never opened by this registry.
    let reply = set_plan_owner(&mut approver, &plan.plan_id, "window-never-opened").await;
    expect_error(reply, "window_not_found");

    // Registered but closed.
    let closed_window = open_and_identify(&h, &mut h.connect().await).await;
    close_window(&mut approver, &closed_window).await;
    let reply = set_plan_owner(&mut approver, &plan.plan_id, &closed_window).await;
    expect_error(reply, "invalid_state");

    let unchanged = expect_plan_detail(get_plan(&mut approver, &plan.plan_id).await);
    assert_ne!(
        unchanged.owner_window.as_deref(),
        Some(closed_window.as_str())
    );
    assert_ne!(
        unchanged.owner_window.as_deref(),
        Some("window-never-opened")
    );

    // A window may never nominate.
    let mut requester = h.connect().await;
    open_and_identify(&h, &mut requester).await;
    let reply = set_plan_owner(&mut requester, &plan.plan_id, &closed_window).await;
    expect_error(reply, "auth_failed");
}

// --- role table: list/save scope and validation --------------------------

#[tokio::test]
async fn list_roles_is_readable_by_a_window_and_an_approver_but_not_external() {
    let dir = unique_temp_dir("plan-assign-roles-scope");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    let roles_path = dir.join("roles.toml");
    let (h, _locks) = start_ledger(
        "plan-assign-roles-scope",
        assign_config(&dir, profiles_dir, Some(roles_path), &[]),
    )
    .await;

    let mut window = h.connect().await;
    open_and_identify(&h, &mut window).await;
    assert_eq!(expect_role_list(list_roles(&mut window).await), Vec::new());

    let mut approver = approver_connection(&h).await;
    assert_eq!(
        expect_role_list(list_roles(&mut approver).await),
        Vec::new()
    );

    let mut external = h.connect().await;
    expect_error(list_roles(&mut external).await, "auth_failed");
}

#[tokio::test]
async fn save_roles_is_approver_only_validates_first_and_a_saved_row_resolves_a_task() {
    let dir = unique_temp_dir("plan-assign-roles-save");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "role-profile", "Role Profile");
    let roles_path = dir.join("roles.toml");
    let (h, _locks) = start_ledger(
        "plan-assign-roles-save",
        assign_config(&dir, profiles_dir, Some(roles_path), &[]),
    )
    .await;

    let mut window = h.connect().await;
    open_and_identify(&h, &mut window).await;
    let reply = save_roles(
        &mut window,
        vec![RoleEntry {
            name: "reviewer".to_string(),
            profile: "role-profile".to_string(),
            model: None,
            effort: None,
        }],
    )
    .await;
    expect_error(reply, "auth_failed");

    let mut approver = approver_connection(&h).await;
    let reply = save_roles(
        &mut approver,
        vec![RoleEntry {
            name: "reviewer".to_string(),
            profile: "profile-does-not-exist".to_string(),
            model: None,
            effort: None,
        }],
    )
    .await;
    assert!(!matches!(reply, DaemonMessage::RoleList { .. }));
    assert_eq!(
        expect_role_list(list_roles(&mut approver).await),
        Vec::new()
    );

    let saved = expect_role_list(
        save_roles(
            &mut approver,
            vec![RoleEntry {
                name: "reviewer".to_string(),
                profile: "role-profile".to_string(),
                model: None,
                effort: None,
            }],
        )
        .await,
    );
    assert_eq!(saved.len(), 1);
    assert_eq!(expect_role_list(list_roles(&mut window).await), saved);

    let plan = create_ok(&mut window, "plan", vec![task_with_role("t1", "reviewer")]).await;
    let t1 = task_info(&plan, "t1");
    assert_eq!(t1.resolved.source, yagura_protocol::AssignmentSource::Role);
    assert_eq!(
        t1.resolved.assignment.as_ref().map(|a| a.profile.as_str()),
        Some("role-profile")
    );
}

#[tokio::test]
async fn list_roles_is_empty_when_no_roles_path_is_resolved() {
    let dir = unique_temp_dir("plan-assign-roles-no-path-list");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    let (h, _locks) = start_ledger(
        "plan-assign-roles-no-path-list",
        assign_config(&dir, profiles_dir, None, &[]),
    )
    .await;

    let mut window = h.connect().await;
    open_and_identify(&h, &mut window).await;
    assert_eq!(expect_role_list(list_roles(&mut window).await), Vec::new());
}

/// The one property already true of the leftover minimal arm before any of
/// this file's handlers exist: `save_roles` answers `unsupported`
/// unconditionally, which happens to already match what a daemon with no
/// resolved role-table path must answer.
#[tokio::test]
async fn save_roles_is_unsupported_when_no_roles_path_is_resolved() {
    let dir = unique_temp_dir("plan-assign-roles-no-path-save");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "role-profile", "Role Profile");
    let (h, _locks) = start_ledger(
        "plan-assign-roles-no-path-save",
        assign_config(&dir, profiles_dir, None, &[]),
    )
    .await;

    let mut approver = approver_connection(&h).await;
    let reply = save_roles(
        &mut approver,
        vec![RoleEntry {
            name: "reviewer".to_string(),
            profile: "role-profile".to_string(),
            model: None,
            effort: None,
        }],
    )
    .await;
    expect_error(reply, "unsupported");
}

#[tokio::test]
async fn a_malformed_roles_file_is_invalid_state_and_never_leaks_its_content() {
    let dir = unique_temp_dir("plan-assign-roles-malformed");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    let roles_path = dir.join("roles.toml");
    std::fs::write(&roles_path, "not-a-valid-roles-document-\u{7}")
        .expect("write malformed roles file");
    let (h, _locks) = start_ledger(
        "plan-assign-roles-malformed",
        assign_config(&dir, profiles_dir, Some(roles_path), &[]),
    )
    .await;

    let mut window = h.connect().await;
    open_and_identify(&h, &mut window).await;
    let message = expect_error(list_roles(&mut window).await, "invalid_state");
    assert!(!message.contains("not-a-valid-roles-document"));
}

// --- open_task_window: authorization ------------------------------------

#[tokio::test]
async fn open_task_window_authorization_matches_the_owner_only_grant() {
    let dir = unique_temp_dir("plan-assign-open-authz");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_argv_profile(&profiles_dir, "assign-profile", "Assign Profile", "--via");
    let (h, _locks) = start_ledger(
        "plan-assign-open-authz",
        assign_config(&dir, profiles_dir, None, &["close_window"]),
    )
    .await;

    let mut owner = h.connect().await;
    let owner_window = open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![
            task_with_profile("t1", "assign-profile"),
            task_with_profile("t2", "assign-profile"),
        ],
    )
    .await;

    let (_, _, bound_window, _) =
        expect_task_window_opened(open_task_window(&mut owner, &plan.plan_id, "t1", None).await);

    let mut bound = h.connect().await;
    identify(&mut bound, &bound_window).await;
    let reply = open_task_window(&mut bound, &plan.plan_id, "t2", None).await;
    expect_error(reply, "not_plan_owner");

    let mut unrelated = h.connect().await;
    open_and_identify(&h, &mut unrelated).await;
    let reply = open_task_window(&mut unrelated, &plan.plan_id, "t2", None).await;
    expect_error(reply, "plan_not_found");

    let mut approver = approver_connection(&h).await;
    let reply = open_task_window(&mut approver, &plan.plan_id, "t2", None).await;
    expect_error(reply, "not_plan_owner");

    let mut external = h.connect().await;
    let reply = open_task_window(&mut external, &plan.plan_id, "t2", None).await;
    expect_error(reply, "auth_failed");

    close_window(&mut approver, &owner_window).await;
    let reply = open_task_window(&mut owner, &plan.plan_id, "t2", None).await;
    expect_error(reply, "not_plan_owner");
}

#[tokio::test]
async fn open_task_window_success_launches_the_resolved_assignment_and_binds_the_task() {
    let dir = unique_temp_dir("plan-assign-open-success");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_argv_profile(&profiles_dir, "assign-profile", "Assign Profile", "--via");
    let (h, _locks) = start_ledger(
        "plan-assign-open-success",
        assign_config(&dir, profiles_dir, None, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task_with_profile_options(
            "t1",
            "assign-profile",
            "fast",
            "high",
        )],
    )
    .await;

    let (plan_id, task_id, window_id, assignment) =
        expect_task_window_opened(open_task_window(&mut owner, &plan.plan_id, "t1", None).await);
    assert_eq!(plan_id, plan.plan_id);
    assert_eq!(task_id, "t1");
    assert_eq!(assignment.source, yagura_protocol::AssignmentSource::Parent);
    assert_eq!(
        assignment.assignment.as_ref().map(|a| a.profile.as_str()),
        Some("assign-profile")
    );

    h.wait_for_scrollback(&window_id, b"<<argv[2]=fast>>", Duration::from_secs(10))
        .await;
    assert!(
        common::contains_subsequence(&h.scrollback(&window_id), b"<<argv[4]=tier=high>>"),
        "the effort option must reach the child argv too"
    );

    let detail = expect_plan_detail(get_plan(&mut owner, &plan.plan_id).await);
    assert_eq!(
        task_info(&detail, "t1").window_id.as_deref(),
        Some(window_id.as_str())
    );
}

#[tokio::test]
async fn open_task_window_is_refused_for_unassigned_or_missing_profile_tasks() {
    let dir = unique_temp_dir("plan-assign-open-refused-codes");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "will-vanish", "Will Vanish");
    let roles_path = dir.join("roles.toml");
    let (h, _locks) = start_ledger(
        "plan-assign-open-refused-codes",
        assign_config(&dir, profiles_dir.clone(), Some(roles_path), &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![
            task("unassigned"),
            task_with_profile("vanishing", "will-vanish"),
        ],
    )
    .await;

    let reply = open_task_window(&mut owner, &plan.plan_id, "unassigned", None).await;
    let message = expect_error(reply, "invalid_state");
    assert_eq!(
        message, "the task has no resolvable assignment",
        "an unassigned task's refusal must name the task's own missing assignment, \
         not the generic window-state wording"
    );

    std::fs::remove_file(profiles_dir.join("will-vanish.toml")).expect("remove the profile file");
    let reply = open_task_window(&mut owner, &plan.plan_id, "vanishing", None).await;
    expect_error(reply, "invalid_argument");
}

/// Already true of the leftover minimal arm: nothing is spawned when
/// `open_task_window` is refused, whatever the eventual refusal code turns
/// out to be.
#[tokio::test]
async fn open_task_window_never_spawns_a_child_when_refused() {
    let dir = unique_temp_dir("plan-assign-open-no-spawn");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    let (h, _locks) = start_ledger(
        "plan-assign-open-no-spawn",
        assign_config(&dir, profiles_dir, None, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(&mut owner, "plan", vec![task("unassigned")]).await;

    let mut approver = approver_connection(&h).await;
    let before = common::call_list_windows(&mut approver).await.len();
    let _ = open_task_window(&mut owner, &plan.plan_id, "unassigned", None).await;
    let after = common::call_list_windows(&mut approver).await.len();
    assert_eq!(
        before, after,
        "a refused open_task_window must never spawn a child"
    );
}

// --- open_task_window: window name defaults to the task title -----------

/// Finds `window_id` in a fresh `list_windows` snapshot and returns its
/// name, the wire-level way this suite already observes window naming
/// elsewhere.
async fn window_name(client: &mut Connection, window_id: &str) -> String {
    common::call_list_windows(client)
        .await
        .into_iter()
        .find(|w| w.window_id == window_id)
        .unwrap_or_else(|| panic!("window {window_id:?} not found in list_windows"))
        .name
}

#[tokio::test]
async fn open_task_window_without_a_name_uses_the_tasks_title() {
    let dir = unique_temp_dir("plan-assign-open-default-name");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "assign-profile", "Assign Profile");
    let (h, _locks) = start_ledger(
        "plan-assign-open-default-name",
        assign_config(&dir, profiles_dir, None, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "assign-profile")],
    )
    .await;

    let (_, _, window_id, _) =
        expect_task_window_opened(open_task_window(&mut owner, &plan.plan_id, "t1", None).await);

    let mut lister = approver_connection(&h).await;
    assert_eq!(
        window_name(&mut lister, &window_id).await,
        "do the thing",
        "omitting `name` must name the window after the task's title, not the profile's \
         display name"
    );
}

#[tokio::test]
async fn open_task_window_with_a_name_uses_it_over_the_tasks_title() {
    let dir = unique_temp_dir("plan-assign-open-explicit-name");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "assign-profile", "Assign Profile");
    let (h, _locks) = start_ledger(
        "plan-assign-open-explicit-name",
        assign_config(&dir, profiles_dir, None, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "assign-profile")],
    )
    .await;

    let (_, _, window_id, _) = expect_task_window_opened(
        open_task_window(&mut owner, &plan.plan_id, "t1", Some("custom window name")).await,
    );

    let mut lister = approver_connection(&h).await;
    assert_eq!(
        window_name(&mut lister, &window_id).await,
        "custom window name",
        "an explicit `name` must still win over the task's title"
    );
}

// --- open_task_window: reopening a bound task -----------------------------

#[tokio::test]
async fn reopening_a_task_is_refused_while_its_window_is_alive_and_allowed_once_it_is_not() {
    let dir = unique_temp_dir("plan-assign-open-reopen");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "assign-profile", "Assign Profile");
    write_self_exiting_profile(&profiles_dir, "self-exit-profile", "Self Exit");
    let (h, _locks) = start_ledger(
        "plan-assign-open-reopen",
        assign_config(&dir, profiles_dir, None, &["close_window"]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![
            task_with_profile("closable", "assign-profile"),
            task_with_profile("self-exiting", "self-exit-profile"),
        ],
    )
    .await;

    let (_, _, window_1, _) = expect_task_window_opened(
        open_task_window(&mut owner, &plan.plan_id, "closable", None).await,
    );
    let reply = open_task_window(&mut owner, &plan.plan_id, "closable", None).await;
    let message = expect_error(reply, "invalid_state");
    assert_eq!(
        message, "the task already has a live window bound to it",
        "reopening a task whose window is still alive must name that specific state, \
         not the generic window-state wording"
    );

    let mut approver = approver_connection(&h).await;
    close_window(&mut approver, &window_1).await;
    let (_, _, window_2, _) = expect_task_window_opened(
        open_task_window(&mut owner, &plan.plan_id, "closable", None).await,
    );
    assert_ne!(window_1, window_2);
    let after_close = expect_plan_detail(get_plan(&mut owner, &plan.plan_id).await);
    assert_eq!(
        task_info(&after_close, "closable").window_id.as_deref(),
        Some(window_2.as_str())
    );

    let (_, _, window_3, _) = expect_task_window_opened(
        open_task_window(&mut owner, &plan.plan_id, "self-exiting", None).await,
    );
    h.wait_until_child_exited(&window_3, Duration::from_secs(10))
        .await;
    let (_, _, window_4, _) = expect_task_window_opened(
        open_task_window(&mut owner, &plan.plan_id, "self-exiting", None).await,
    );
    assert_ne!(window_3, window_4);
}

// --- open_task_window: the operator's override wins -----------------------

#[tokio::test]
async fn open_task_window_uses_the_operator_override_over_the_planning_windows_proposal() {
    let dir = unique_temp_dir("plan-assign-open-override-wins");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_argv_profile(&profiles_dir, "profile-a", "Profile A", "--via-a");
    write_argv_profile(&profiles_dir, "profile-b", "Profile B", "--via-b");
    let (h, _locks) = start_ledger(
        "plan-assign-open-override-wins",
        assign_config(&dir, profiles_dir, None, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "profile-a")],
    )
    .await;

    let mut approver = approver_connection(&h).await;
    expect_plan_detail(
        set_task_assignment(
            &mut approver,
            &plan.plan_id,
            "t1",
            TaskAssignment {
                profile: "profile-b".to_string(),
                model: Some("override-model".to_string()),
                effort: None,
            },
        )
        .await,
    );

    let (_, _, window_id, assignment) =
        expect_task_window_opened(open_task_window(&mut owner, &plan.plan_id, "t1", None).await);
    assert_eq!(assignment.source, yagura_protocol::AssignmentSource::User);
    assert_eq!(
        assignment.assignment.as_ref().map(|a| a.profile.as_str()),
        Some("profile-b")
    );
    h.wait_for_scrollback(
        &window_id,
        b"<<argv[2]=override-model>>",
        Duration::from_secs(10),
    )
    .await;
    assert!(
        common::contains_subsequence(&h.scrollback(&window_id), b"<<argv[1]=--via-b>>"),
        "the override's own flag must reach the child argv"
    );
    assert!(
        !common::contains_subsequence(&h.scrollback(&window_id), b"--via-a"),
        "the overridden profile's own flag must never reach the child argv"
    );
}

// --- a bound (non-owner) task window can read but not write ---------------

#[tokio::test]
async fn a_window_bound_by_open_task_window_can_read_but_not_write_the_plan() {
    let dir = unique_temp_dir("plan-assign-bound-window-scope");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "assign-profile", "Assign Profile");
    let (h, _locks) = start_ledger(
        "plan-assign-bound-window-scope",
        assign_config(&dir, profiles_dir, None, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "assign-profile")],
    )
    .await;
    let (_, _, window_id, _) =
        expect_task_window_opened(open_task_window(&mut owner, &plan.plan_id, "t1", None).await);

    let mut bound = h.connect().await;
    identify(&mut bound, &window_id).await;

    let read = expect_plan_detail(get_plan(&mut bound, &plan.plan_id).await);
    assert_eq!(read.plan_id, plan.plan_id);
    let listed = match list_plans(&mut bound).await {
        DaemonMessage::PlanList { plans } => plans,
        other => panic!("expected plan_list, got {other:?}"),
    };
    assert!(listed.iter().any(|p| p.plan_id == plan.plan_id));

    let reply = update_plan(&mut bound, &plan.plan_id, vec![task("t1")]).await;
    expect_error(reply, "not_plan_owner");
    let reply = delete_plan(&mut bound, &plan.plan_id).await;
    expect_error(reply, "not_plan_owner");
}

// --- open_task_window: binding failure is cleaned up ----------------------

/// Installs a trigger on `plan_task` that fails any write to `window_id`, so
/// a case can exercise the branch where a task window is opened but the
/// binding record could not be written -- without adding a test-only hook to
/// production code.
fn install_binding_failure_trigger(db_path: &Path) {
    let conn =
        rusqlite::Connection::open(db_path).expect("open a second connection to the plan store");
    conn.execute_batch(
        "CREATE TRIGGER block_task_window_bind \
         BEFORE UPDATE OF window_id ON plan_task \
         BEGIN SELECT RAISE(ABORT, 'induced test failure'); END;",
    )
    .expect("install the binding-failure trigger");
}

fn remove_binding_failure_trigger(db_path: &Path) {
    let conn =
        rusqlite::Connection::open(db_path).expect("open a second connection to the plan store");
    conn.execute_batch("DROP TRIGGER block_task_window_bind;")
        .expect("remove the binding-failure trigger");
}

#[tokio::test]
async fn a_failed_binding_write_leaves_the_opened_window_dead_and_the_task_unbound() {
    let dir = unique_temp_dir("plan-assign-open-bind-failure");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "assign-profile", "Assign Profile");
    let db_path = dir.join("plan.db");
    let mut config = assign_config(&dir, profiles_dir, None, &[]);
    config.store_db_path_resolved = Some(db_path.clone());
    let (h, _locks) = start_ledger("plan-assign-open-bind-failure", config).await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "assign-profile")],
    )
    .await;

    install_binding_failure_trigger(&db_path);

    let mut approver = approver_connection(&h).await;
    let before = common::call_list_windows(&mut approver).await;

    let reply = open_task_window(&mut owner, &plan.plan_id, "t1", None).await;
    assert!(
        !matches!(reply, DaemonMessage::TaskWindowOpened { .. }),
        "a binding write that fails must not report success"
    );

    let after = open_task_window_windows_snapshot(&mut approver).await;
    let new_windows: Vec<&yagura_protocol::WindowInfo> = after
        .iter()
        .filter(|w| !before.iter().any(|b| b.window_id == w.window_id))
        .collect();
    assert_eq!(
        new_windows.len(),
        1,
        "a binding failure must still leave the spawned child window registered, so it can be \
         diagnosed, but must not leave a second one behind on retry"
    );
    let orphan_id = new_windows[0].window_id.clone();
    h.wait_until_child_exited(&orphan_id, Duration::from_secs(10))
        .await;

    let detail = expect_plan_detail(get_plan(&mut owner, &plan.plan_id).await);
    assert_eq!(
        task_info(&detail, "t1").window_id,
        None,
        "a task must not show a binding that failed to write"
    );

    remove_binding_failure_trigger(&db_path);
    let (_, _, window_id, _) =
        expect_task_window_opened(open_task_window(&mut owner, &plan.plan_id, "t1", None).await);
    let recovered = expect_plan_detail(get_plan(&mut owner, &plan.plan_id).await);
    assert_eq!(
        task_info(&recovered, "t1").window_id.as_deref(),
        Some(window_id.as_str())
    );
}

async fn open_task_window_windows_snapshot(
    client: &mut Connection,
) -> Vec<yagura_protocol::WindowInfo> {
    common::call_list_windows(client).await
}

// --- open_task_window: concurrent opens against the same task serialize --

#[tokio::test]
async fn two_concurrent_opens_of_the_same_task_produce_exactly_one_success() {
    let dir = unique_temp_dir("plan-assign-open-race");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "assign-profile", "Assign Profile");
    let (h, _locks) = start_ledger(
        "plan-assign-open-race",
        assign_config(&dir, profiles_dir, None, &[]),
    )
    .await;

    let mut owner_a = h.connect().await;
    let window_a = open_and_identify(&h, &mut owner_a).await;
    let plan = create_ok(
        &mut owner_a,
        "plan",
        vec![task_with_profile("t1", "assign-profile")],
    )
    .await;

    // A second connection identifying as the same owner window: a realistic
    // reconnect, not a new window.
    let mut owner_a2 = h.connect().await;
    identify(&mut owner_a2, &window_a).await;

    // A plain, unregistered connection for the window count: a registered
    // approver would also receive the `plan_changed` push the winning open
    // triggers, racing that push against the `list_windows` reply on the
    // same connection.
    let mut lister = h.connect().await;
    let before = common::call_list_windows(&mut lister).await.len();

    send(
        &mut owner_a,
        &ClientMessage::OpenTaskWindow {
            plan_id: plan.plan_id.clone(),
            task_id: "t1".to_string(),
            name: None,
        },
    )
    .await;
    send(
        &mut owner_a2,
        &ClientMessage::OpenTaskWindow {
            plan_id: plan.plan_id.clone(),
            task_id: "t1".to_string(),
            name: None,
        },
    )
    .await;

    let reply_a = read_ledger_reply(&mut owner_a, Duration::from_secs(10)).await;
    let reply_a2 = read_ledger_reply(&mut owner_a2, Duration::from_secs(10)).await;

    let opened = [&reply_a, &reply_a2]
        .into_iter()
        .filter(|msg| matches!(msg, DaemonMessage::TaskWindowOpened { .. }))
        .count();
    let refused = [&reply_a, &reply_a2]
        .into_iter()
        .filter(|msg| matches!(msg, DaemonMessage::Error { code, .. } if code == "invalid_state"))
        .count();
    assert_eq!(
        opened, 1,
        "exactly one of the two concurrent opens must succeed"
    );
    assert_eq!(refused, 1, "the other must be refused as already bound");

    let after = common::call_list_windows(&mut lister).await.len();
    assert_eq!(after, before + 1, "the race must add exactly one window");
}

// --- push notifications ---------------------------------------------------

#[tokio::test]
async fn every_structural_assignment_change_pushes_plan_changed() {
    let dir = unique_temp_dir("plan-assign-push");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "assign-profile", "Assign Profile");
    let (h, _locks) = start_ledger(
        "plan-assign-push",
        assign_config(&dir, profiles_dir, None, &[]),
    )
    .await;

    let mut watcher = approver_connection(&h).await;

    let mut owner = h.connect().await;
    let owner_window = open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "assign-profile")],
    )
    .await;
    expect_plan_changed(&mut watcher, &plan.plan_id).await;

    let mut actor = approver_connection(&h).await;
    expect_plan_detail(
        set_task_assignment(
            &mut actor,
            &plan.plan_id,
            "t1",
            TaskAssignment {
                profile: "assign-profile".to_string(),
                model: None,
                effort: None,
            },
        )
        .await,
    );
    expect_plan_changed(&mut watcher, &plan.plan_id).await;

    expect_plan_detail(clear_task_assignment(&mut actor, &plan.plan_id, "t1").await);
    expect_plan_changed(&mut watcher, &plan.plan_id).await;

    let mut second_window_conn = h.connect().await;
    let second_window = open_and_identify(&h, &mut second_window_conn).await;
    expect_plan_detail(set_plan_owner(&mut actor, &plan.plan_id, &second_window).await);
    expect_plan_changed(&mut watcher, &plan.plan_id).await;

    let mut second_owner = h.connect().await;
    identify(&mut second_owner, &second_window).await;
    expect_task_window_opened(open_task_window(&mut second_owner, &plan.plan_id, "t1", None).await);
    expect_plan_changed(&mut watcher, &plan.plan_id).await;

    let _ = owner_window;
}

async fn expect_plan_changed(client: &mut Connection, plan_id: &str) {
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::PlanChanged { plan_id: pushed } => assert_eq!(pushed, plan_id),
        other => panic!("expected plan_changed, got {other:?}"),
    }
}

/// Already true of the leftover minimal arm: `save_roles` never pushes,
/// since it never reaches a write at all yet.
#[tokio::test]
async fn saving_roles_never_pushes_plan_changed() {
    let dir = unique_temp_dir("plan-assign-roles-no-push");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "role-profile", "Role Profile");
    let roles_path = dir.join("roles.toml");
    let (h, _locks) = start_ledger(
        "plan-assign-roles-no-push",
        assign_config(&dir, profiles_dir, Some(roles_path), &[]),
    )
    .await;

    let mut watcher = approver_connection(&h).await;
    let mut actor = approver_connection(&h).await;
    let _ = save_roles(
        &mut actor,
        vec![RoleEntry {
            name: "reviewer".to_string(),
            profile: "role-profile".to_string(),
            model: None,
            effort: None,
        }],
    )
    .await;

    expect_no_message_within(&mut watcher, Duration::from_millis(300)).await;
}

// --- a held plan lock delays every operator write ----------------------

#[tokio::test]
async fn a_held_plan_lock_delays_every_operator_write() {
    let dir = unique_temp_dir("plan-assign-lock-delays-operator");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "assign-profile", "Assign Profile");
    let (h, locks) = start_ledger(
        "plan-assign-lock-delays-operator",
        assign_config(&dir, profiles_dir, None, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "assign-profile")],
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let held = locks.lock(&plan.plan_id).await;
    send(
        &mut approver,
        &ClientMessage::SetTaskAssignment {
            plan_id: plan.plan_id.clone(),
            task_id: "t1".to_string(),
            assignment: TaskAssignment {
                profile: "assign-profile".to_string(),
                model: None,
                effort: None,
            },
        },
    )
    .await;
    expect_no_message_within(&mut approver, Duration::from_millis(300)).await;
    drop(held);
    expect_plan_detail(read_ledger_reply(&mut approver, Duration::from_secs(10)).await);
}

// --- a nomination target's liveness is re-checked inside the lock -------

#[tokio::test]
async fn a_nomination_targets_liveness_is_checked_inside_the_held_lock() {
    let dir = unique_temp_dir("plan-assign-nominate-lock");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    let (h, locks) = start_ledger(
        "plan-assign-nominate-lock",
        assign_config(&dir, profiles_dir, None, &["close_window"]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(&mut owner, "plan", vec![task("t1")]).await;

    let mut candidate_conn = h.connect().await;
    let candidate = open_and_identify(&h, &mut candidate_conn).await;

    let mut approver = approver_connection(&h).await;
    let held = locks.lock(&plan.plan_id).await;

    send(
        &mut approver,
        &ClientMessage::SetPlanOwner {
            plan_id: plan.plan_id.clone(),
            window_id: candidate.clone(),
        },
    )
    .await;
    expect_no_message_within(&mut approver, Duration::from_millis(300)).await;

    let mut closer = approver_connection(&h).await;
    close_window(&mut closer, &candidate).await;

    drop(held);
    let reply = read_ledger_reply(&mut approver, Duration::from_secs(10)).await;
    expect_error(reply, "invalid_state");

    let unchanged = expect_plan_detail(get_plan(&mut closer, &plan.plan_id).await);
    assert_ne!(unchanged.owner_window.as_deref(), Some(candidate.as_str()));
}
