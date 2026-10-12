#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the role table's `default_role` over IPC -- saving and
//! listing it, and how a task that names no role (or a role the table lacks)
//! resolves in `get_plan` and launches in `open_task_window`.
//!
//! Every config here states `plan_approval_required: false` so these cases
//! keep opening task windows directly whatever the built-in default becomes.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{
    Harness, approver_base_config, expect_error, identify, policy, read_response,
    register_approver, send, unique_temp_dir, write_profile,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::plan_permit::LockTable;
use yagura_daemon::roles::read_roles;
use yagura_protocol::{
    AssignmentSource, ClientMessage, DaemonMessage, PlanInfo, PlanTaskInfo, PlanTaskSpec,
    ResolvedAssignment, RoleEntry,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{argv_probe_profile_source, ready_child_profile_source};

const DEFAULT_PROFILE: &str = "default-profile";
const DEFAULT_FLAG: &str = "--from-default";
const REVIEW_PROFILE: &str = "review-profile";

fn write_argv_profile(dir: &Path, id: &str, display_name: &str, model_flag: &str) {
    let tables = format!("\n[model]\nargs = [\"{model_flag}\", \"{{value}}\"]\n");
    write_profile(dir, id, &argv_probe_profile_source(display_name, &tables));
}

fn roles_config(dir: &Path, profiles_dir: PathBuf, roles_path: PathBuf) -> DaemonConfig {
    let mut config = approver_base_config();
    config.approval_policy = Some(policy(&["open_window"]));
    config.store_db_path_resolved = Some(dir.join("plan.db"));
    config.profiles_dir = Some(profiles_dir);
    config.roles_path = Some(roles_path);
    config.plan_approval_required = false;
    config
}

struct Fixture {
    harness: Harness,
    _locks: Arc<LockTable>,
    roles_path: PathBuf,
}

async fn start(tag: &str) -> Fixture {
    let dir = unique_temp_dir(tag);
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_argv_profile(
        &profiles_dir,
        DEFAULT_PROFILE,
        "Default Profile",
        DEFAULT_FLAG,
    );
    write_profile(
        &profiles_dir,
        REVIEW_PROFILE,
        &ready_child_profile_source("Review Profile"),
    );
    let roles_path = dir.join("roles.toml");
    let (harness, locks) =
        Harness::start_with_ledger(tag, roles_config(&dir, profiles_dir, roles_path.clone())).await;
    Fixture {
        harness,
        _locks: locks,
        roles_path,
    }
}

fn rows() -> Vec<RoleEntry> {
    vec![
        RoleEntry {
            name: "impl".to_string(),
            profile: DEFAULT_PROFILE.to_string(),
            model: Some("fast".to_string()),
            effort: None,
        },
        RoleEntry {
            name: "review".to_string(),
            profile: REVIEW_PROFILE.to_string(),
            model: None,
            effort: None,
        },
    ]
}

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

async fn request(client: &mut Connection, message: &ClientMessage) -> DaemonMessage {
    send(client, message).await;
    read_ledger_reply(client, Duration::from_secs(10)).await
}

async fn approver_connection(h: &Harness) -> Connection {
    let mut client = h.connect().await;
    register_approver(&mut client).await;
    client
}

async fn window_connection(h: &Harness) -> Connection {
    let mut client = h.connect().await;
    let window_id = h
        .open(yagura_test_support::process::echo_child_command(), None)
        .await;
    identify(&mut client, &window_id).await;
    client
}

async fn save_roles(client: &mut Connection, default_role: Option<&str>) -> DaemonMessage {
    request(
        client,
        &ClientMessage::SaveRoles {
            roles: rows(),
            default_role: default_role.map(str::to_string),
        },
    )
    .await
}

fn expect_role_list(msg: DaemonMessage) -> (Vec<RoleEntry>, Option<String>) {
    match msg {
        DaemonMessage::RoleList {
            roles,
            default_role,
        } => (roles, default_role),
        other => panic!("expected role_list, got {other:?}"),
    }
}

fn expect_plan_detail(msg: DaemonMessage) -> PlanInfo {
    match msg {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

fn expect_opened(msg: DaemonMessage) -> (String, ResolvedAssignment) {
    match msg {
        DaemonMessage::TaskWindowOpened {
            window_id,
            assignment,
            ..
        } => (window_id, assignment),
        other => panic!("expected task_window_opened, got {other:?}"),
    }
}

fn task_info<'a>(plan: &'a PlanInfo, task_id: &str) -> &'a PlanTaskInfo {
    plan.tasks
        .iter()
        .find(|t| t.task_id == task_id)
        .unwrap_or_else(|| panic!("task {task_id:?} not found in plan {plan:?}"))
}

fn task(task_id: &str, role: Option<&str>) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: task_id.to_string(),
        title: "do the thing".to_string(),
        role: role.map(str::to_string),
        profile: None,
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: None,
    }
}

fn source_and_profile(resolved: &ResolvedAssignment) -> (AssignmentSource, Option<&str>) {
    (
        resolved.source,
        resolved.assignment.as_ref().map(|a| a.profile.as_str()),
    )
}

#[tokio::test]
// Reads the saved roles.toml back directly to check what the daemon wrote.
#[allow(clippy::disallowed_methods)]
async fn an_approver_saves_a_default_role_and_it_is_listed_and_written() {
    let f = start("roles-default-save").await;
    let mut approver = approver_connection(&f.harness).await;

    let (saved_rows, saved_default) =
        expect_role_list(save_roles(&mut approver, Some("impl")).await);
    assert_eq!(saved_rows, rows());
    assert_eq!(saved_default.as_deref(), Some("impl"));

    let (_, listed_default) =
        expect_role_list(request(&mut approver, &ClientMessage::ListRoles).await);
    assert_eq!(listed_default.as_deref(), Some("impl"));

    let on_disk = read_roles(&f.roles_path).expect("the saved roles.toml must read back");
    assert_eq!(on_disk.default_role.as_deref(), Some("impl"));
}

#[tokio::test]
async fn a_default_role_the_table_lacks_is_invalid_argument_and_leaves_the_file_untouched() {
    let f = start("roles-default-unknown").await;
    let mut approver = approver_connection(&f.harness).await;
    expect_role_list(save_roles(&mut approver, Some("impl")).await);
    let before = std::fs::read(&f.roles_path).expect("the saved roles.toml must be readable");

    let message = expect_error(
        save_roles(&mut approver, Some("ghost")).await,
        "invalid_argument",
    );
    assert!(
        !message.contains("ghost"),
        "the refusal must not echo the rejected name: {message}"
    );
    assert_eq!(
        std::fs::read(&f.roles_path).expect("roles.toml must still be readable"),
        before,
        "a refused save must not touch roles.toml"
    );
    let (_, listed_default) =
        expect_role_list(request(&mut approver, &ClientMessage::ListRoles).await);
    assert_eq!(listed_default.as_deref(), Some("impl"));
}

#[tokio::test]
async fn get_plan_resolves_tasks_without_a_known_role_to_the_default_row() {
    let f = start("roles-default-get-plan").await;
    let mut approver = approver_connection(&f.harness).await;
    expect_role_list(save_roles(&mut approver, Some("impl")).await);

    let mut owner = window_connection(&f.harness).await;
    let created = expect_plan_detail(
        request(
            &mut owner,
            &ClientMessage::CreatePlan {
                title: "plan".to_string(),
                tasks: vec![
                    task("no-role", None),
                    task("unknown-role", Some("docs")),
                    task("known-role", Some("review")),
                ],
            },
        )
        .await,
    );
    let plan = expect_plan_detail(
        request(
            &mut owner,
            &ClientMessage::GetPlan {
                plan_id: created.plan_id.clone(),
            },
        )
        .await,
    );

    assert_eq!(
        source_and_profile(&task_info(&plan, "no-role").resolved),
        (AssignmentSource::Default, Some(DEFAULT_PROFILE))
    );
    assert_eq!(
        source_and_profile(&task_info(&plan, "unknown-role").resolved),
        (AssignmentSource::Default, Some(DEFAULT_PROFILE))
    );
    assert_eq!(
        source_and_profile(&task_info(&plan, "known-role").resolved),
        (AssignmentSource::Role, Some(REVIEW_PROFILE))
    );
}

#[tokio::test]
async fn open_task_window_launches_the_default_rows_profile_for_tasks_without_a_known_role() {
    let f = start("roles-default-open").await;
    let mut approver = approver_connection(&f.harness).await;
    expect_role_list(save_roles(&mut approver, Some("impl")).await);

    let mut owner = window_connection(&f.harness).await;
    let plan = expect_plan_detail(
        request(
            &mut owner,
            &ClientMessage::CreatePlan {
                title: "plan".to_string(),
                tasks: vec![task("no-role", None), task("unknown-role", Some("docs"))],
            },
        )
        .await,
    );

    for task_id in ["no-role", "unknown-role"] {
        let (window_id, assignment) = expect_opened(
            request(
                &mut owner,
                &ClientMessage::OpenTaskWindow {
                    plan_id: plan.plan_id.clone(),
                    task_id: task_id.to_string(),
                    name: None,
                },
            )
            .await,
        );
        assert_eq!(
            source_and_profile(&assignment),
            (AssignmentSource::Default, Some(DEFAULT_PROFILE)),
            "task {task_id}"
        );
        f.harness
            .wait_for_scrollback(
                &window_id,
                format!("<<argv[1]={DEFAULT_FLAG}>>").as_bytes(),
                Duration::from_secs(10),
            )
            .await;
        assert!(
            common::contains_subsequence(&f.harness.scrollback(&window_id), b"<<argv[2]=fast>>"),
            "the default row's model must reach the child argv for task {task_id}"
        );
    }
}

#[tokio::test]
async fn a_roles_file_whose_default_names_no_row_leaves_every_task_unassigned() {
    let f = start("roles-default-unreadable").await;
    std::fs::write(
        &f.roles_path,
        format!(
            "schema = 1\ndefault_role = \"ghost\"\n\n[[role]]\nname = \"impl\"\nprofile = \
             \"{DEFAULT_PROFILE}\"\n"
        ),
    )
    .expect("write an inconsistent roles.toml");

    let mut owner = window_connection(&f.harness).await;
    let plan = expect_plan_detail(
        request(
            &mut owner,
            &ClientMessage::CreatePlan {
                title: "plan".to_string(),
                tasks: vec![task("no-role", None), task("known-role", Some("impl"))],
            },
        )
        .await,
    );

    for task_id in ["no-role", "known-role"] {
        assert_eq!(
            source_and_profile(&task_info(&plan, task_id).resolved),
            (AssignmentSource::Unassigned, None),
            "task {task_id}: a table that fails validation must supply neither a role nor a default"
        );
    }
}
