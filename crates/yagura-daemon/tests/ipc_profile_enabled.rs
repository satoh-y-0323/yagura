#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the daemon IPC surface for a profile's `enabled` switch
//! -- rejecting the three profile-addressed window-open paths and a
//! disabled profile's model-choices listing with `profile_disabled`,
//! `list_profiles` reporting the state rather than filtering it, the
//! plan-ledger paths that must keep working against a disabled profile, and
//! the approver-only `SetProfileEnabled` request itself.
//!
//! Every fixture profile is written by the test itself, the same discipline
//! `ipc_profile.rs`'s own module doc documents; availability is always
//! decided against an injected `program_lookup`, never against what happens
//! to be installed on the machine running the suite.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{
    Harness, approver_base_config, call_list_windows, call_send_handoff_from_profile, empty_lookup,
    expect_error, handoff_profile_config, identify, policy, read_response, register_approver, send,
    unique_temp_dir, write_profile,
};

use yagura_daemon::DaemonConfig;
use yagura_daemon::plan_permit::LockTable;
use yagura_daemon::profile::ProfileOs;
use yagura_daemon::roles::RolesError;
use yagura_os::program::ProgramLookup;
use yagura_protocol::{
    ClientMessage, DaemonMessage, ModelChoice, ModelChoiceSource, PlanInfo, PlanTaskSpec,
    ProfileInfo, RoleEntry,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::echo_child_path;

// --- fixture profiles --------------------------------------------------------

/// A profile whose `[command]` launches the real `echo_child` fixture binary
/// by absolute path, switched off when `enabled` is `false`. `None` omits
/// the key entirely (the default, `true`).
fn echo_child_profile_source(display_name: &str, enabled: Option<bool>) -> String {
    let path = echo_child_path();
    let enabled_line = match enabled {
        Some(value) => format!("enabled = {value}\n"),
        None => String::new(),
    };
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\n{enabled_line}\n[command]\ndefault = \
         ['{}']\n",
        path.display()
    )
}

/// A profile whose `[command]` names an argv for every operating system
/// except the host's own -- the same shape `ipc_profile.rs`'s
/// `profile_source_with_no_argv_for_host` uses -- so it never has anything
/// to launch here, whatever its `enabled` key says.
fn profile_source_with_no_argv_for_host(display_name: &str) -> String {
    let other_os_keys = if cfg!(windows) {
        "macos = [\"macos-only\"]\nlinux = [\"linux-only\"]\n"
    } else if cfg!(target_os = "macos") {
        "windows = [\"windows-only\"]\nlinux = [\"linux-only\"]\n"
    } else {
        "windows = [\"windows-only\"]\nmacos = [\"macos-only\"]\n"
    };
    format!("schema = 1\ndisplay_name = \"{display_name}\"\n\n[command]\n{other_os_keys}")
}

// --- injected resolution environment (mirrors ipc_profile.rs) --------------

fn file_exists(path: &Path) -> bool {
    path.is_file()
}

fn config_with(profiles_dir: Option<PathBuf>, lookup: ProgramLookup) -> DaemonConfig {
    DaemonConfig {
        profiles_dir,
        host_os: ProfileOs::host(),
        program_lookup: lookup,
        program_is_executable: file_exists,
        default_shell: None,
        detection_dir: None,
        ..approver_base_config()
    }
}

/// A ledger-backed config for the role-table / `create_plan` cases, the same
/// shape `ipc_plan_assign.rs`'s own `assign_config` builds: `plan_approval_required`
/// is off, so a plan created here does not start pending.
fn ledger_config(dir: &Path, profiles_dir: PathBuf, roles_path: Option<PathBuf>) -> DaemonConfig {
    let mut config = approver_base_config();
    config.approval_policy = Some(policy(&[]));
    config.store_db_path_resolved = Some(dir.join("plan.db"));
    config.profiles_dir = Some(profiles_dir);
    config.roles_path = roles_path;
    config.plan_approval_required = false;
    config
}

// --- IPC round trips ---------------------------------------------------------

async fn call_list_profiles(client: &mut Connection) -> Vec<ProfileInfo> {
    send(client, &ClientMessage::ListProfiles).await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::ProfileList { profiles, .. } => profiles,
        other => panic!("expected profile_list, got {other:?}"),
    }
}

async fn call_open_window_from_profile(client: &mut Connection, profile: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::OpenWindowFromProfile {
            profile: profile.to_string(),
            cwd: None,
            name: None,
            model: None,
            effort: None,
            on_name_taken: Default::default(),
        },
    )
    .await;
    read_response(client, Duration::from_secs(5)).await
}

async fn call_set_profile_enabled(
    client: &mut Connection,
    profile: &str,
    enabled: bool,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::SetProfileEnabled {
            profile: profile.to_string(),
            enabled,
        },
    )
    .await;
    read_response(client, Duration::from_secs(5)).await
}

fn expect_window_opened(msg: DaemonMessage) -> String {
    match msg {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected window_opened, got {other:?}"),
    }
}

fn find_profile<'a>(profiles: &'a [ProfileInfo], id: &str) -> &'a ProfileInfo {
    profiles
        .iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("profile {id:?} not found in listing: {profiles:?}"))
}

async fn approver_connection(h: &Harness) -> Connection {
    let mut client = h.connect().await;
    register_approver(&mut client).await;
    client
}

// =============================================================================
// OpenWindowFromProfile against a disabled profile, with an enabled
// control of the same content.
// =============================================================================

#[tokio::test]
async fn open_window_from_profile_with_a_disabled_profile_is_profile_disabled_and_opens_no_window()
{
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-open-window");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "disabled",
        &echo_child_profile_source("Disabled", Some(false)),
    );
    write_profile(
        &profiles_dir,
        "enabled",
        &echo_child_profile_source("Enabled", Some(true)),
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("profile-enabled-open-window", config).await;
    let mut client = harness.connect().await;

    let before = call_list_windows(&mut client).await.len();
    let response = call_open_window_from_profile(&mut client, "disabled").await;
    expect_error(response, "profile_disabled");
    let after = call_list_windows(&mut client).await.len();
    assert_eq!(
        before, after,
        "a profile_disabled refusal must not open a window"
    );

    // Control: the same content, enabled, opens.
    let response = call_open_window_from_profile(&mut client, "enabled").await;
    expect_window_opened(response);

    harness.registry.terminate_all().await;
}

// =============================================================================
// OpenTaskWindow, resolved through an explicit role and through the
// default role, each naming a disabled profile.
// =============================================================================

fn task_with_role(task_id: &str, role: Option<&str>) -> PlanTaskSpec {
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

async fn create_plan(client: &mut Connection, title: &str, tasks: Vec<PlanTaskSpec>) -> PlanInfo {
    send(
        client,
        &ClientMessage::CreatePlan {
            title: title.to_string(),
            tasks,
            ticket: None,
        },
    )
    .await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

async fn save_roles(
    client: &mut Connection,
    roles: Vec<RoleEntry>,
    default_role: Option<&str>,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::SaveRoles {
            roles,
            default_role: default_role.map(str::to_string),
        },
    )
    .await;
    read_response(client, Duration::from_secs(5)).await
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
    read_response(client, Duration::from_secs(10)).await
}

async fn get_plan(client: &mut Connection, plan_id: &str) -> PlanInfo {
    send(
        client,
        &ClientMessage::GetPlan {
            plan_id: plan_id.to_string(),
        },
    )
    .await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

fn task_window_id(plan: &PlanInfo, task_id: &str) -> Option<String> {
    plan.tasks
        .iter()
        .find(|task| task.task_id == task_id)
        .unwrap_or_else(|| panic!("task {task_id:?} not found in plan {plan:?}"))
        .window_id
        .clone()
}

/// A role-resolved and a default-role-resolved task both naming a disabled
/// profile: both refusals must be `profile_disabled`, open no window and
/// leave the task's own binding unwritten.
#[tokio::test]
async fn open_task_window_for_a_disabled_profile_is_profile_disabled_and_does_not_bind_via_role_or_default_role()
 {
    let dir = unique_temp_dir("profile-enabled-open-task");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_profile(
        &profiles_dir,
        "disabled",
        &echo_child_profile_source("Disabled", Some(false)),
    );
    let roles_path = dir.join("roles.toml");
    let (h, _locks): (Harness, std::sync::Arc<LockTable>) = Harness::start_with_ledger(
        "profile-enabled-open-task",
        ledger_config(&dir, profiles_dir, Some(roles_path)),
    )
    .await;

    let mut approver = approver_connection(&h).await;
    let saved = save_roles(
        &mut approver,
        vec![RoleEntry {
            name: "reviewer".to_string(),
            profile: "disabled".to_string(),
            model: None,
            effort: None,
        }],
        Some("reviewer"),
    )
    .await;
    assert!(
        matches!(saved, DaemonMessage::RoleList { .. }),
        "the role table must save even though its row names a disabled profile: {saved:?}"
    );

    let mut owner = h.connect().await;
    let owner_window = h
        .open(yagura_test_support::process::echo_child_command(), None)
        .await;
    identify(&mut owner, &owner_window).await;

    let plan = create_plan(
        &mut owner,
        "plan",
        vec![
            task_with_role("via-role", Some("reviewer")),
            task_with_role("via-default", None),
        ],
    )
    .await;

    let before = call_list_windows(&mut owner).await.len();

    let reply = open_task_window(&mut owner, &plan.plan_id, "via-role").await;
    expect_error(reply, "profile_disabled");
    let reply = open_task_window(&mut owner, &plan.plan_id, "via-default").await;
    expect_error(reply, "profile_disabled");

    let after = call_list_windows(&mut owner).await.len();
    assert_eq!(
        before, after,
        "a profile_disabled refusal must not open a window, whether the assignment came \
         through the named role or the default role"
    );

    let detail = get_plan(&mut owner, &plan.plan_id).await;
    assert_eq!(
        task_window_id(&detail, "via-role"),
        None,
        "a refused open_task_window must never bind the task"
    );
    assert_eq!(task_window_id(&detail, "via-default"), None);
}

// =============================================================================
// send_handoff addressed to a disabled profile.
// =============================================================================

#[tokio::test]
async fn send_handoff_to_a_disabled_profile_is_profile_disabled_without_any_approver() {
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-handoff");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "disabled",
        &echo_child_profile_source("Disabled", Some(false)),
    );

    // No auto_approve, no playbook: PolicySource::NotConfigured. The
    // profile-disabled refusal (inside plan_from_profile) must happen before
    // the pre-launch policy judgement, so no approver is needed for this to
    // fail closed -- the same discipline `ipc_profile.rs`'s own
    // `send_handoff_from_a_profile_with_a_nonexistent_cwd_is_invalid_argument_without_any_approver`
    // documents.
    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        None,
        30,
    );
    let h = Harness::start("profile-enabled-handoff", config).await;
    let mut client = h.connect().await;

    let before = call_list_windows(&mut client).await.len();
    let msg =
        call_send_handoff_from_profile(&mut client, "disabled", None, None, "src/foo.rs", "look")
            .await;
    expect_error(msg, "profile_disabled");
    let after = call_list_windows(&mut client).await.len();
    assert_eq!(
        before, after,
        "a profile_disabled refusal must not open a window"
    );
}

// =============================================================================
// list_profiles reports the enabled state rather than filtering it out.
// =============================================================================

#[tokio::test]
async fn list_profiles_reports_a_disabled_profile_with_enabled_false_instead_of_excluding_it() {
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-listing");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "disabled",
        &echo_child_profile_source("Disabled", Some(false)),
    );
    write_profile(
        &profiles_dir,
        "enabled",
        &echo_child_profile_source("Enabled", Some(true)),
    );
    write_profile(
        &profiles_dir,
        "default-enabled",
        &echo_child_profile_source("Default Enabled", None),
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("profile-enabled-listing", config).await;
    let mut client = harness.connect().await;

    let profiles = call_list_profiles(&mut client).await;

    let ids: Vec<&str> = profiles.iter().map(|p| p.id.as_str()).collect();
    assert!(
        ids.contains(&"disabled"),
        "a disabled profile must still be listed, not excluded: {profiles:?}"
    );
    assert!(!find_profile(&profiles, "disabled").enabled);
    assert!(find_profile(&profiles, "enabled").enabled);
    assert!(
        find_profile(&profiles, "default-enabled").enabled,
        "a profile with no enabled key must be reported enabled"
    );
}

// =============================================================================
// The plan ledger paths that must keep working against a disabled
// profile, and the one that must still refuse an operating-system-excluded
// profile.
// =============================================================================

#[tokio::test]
async fn saving_a_role_table_naming_a_disabled_profile_succeeds() {
    let dir = unique_temp_dir("profile-enabled-role-save-disabled");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_profile(
        &profiles_dir,
        "disabled",
        &echo_child_profile_source("Disabled", Some(false)),
    );
    let roles_path = dir.join("roles.toml");
    let (h, _locks) = Harness::start_with_ledger(
        "profile-enabled-role-save-disabled",
        ledger_config(&dir, profiles_dir, Some(roles_path)),
    )
    .await;

    let mut approver = approver_connection(&h).await;
    let reply = save_roles(
        &mut approver,
        vec![RoleEntry {
            name: "reviewer".to_string(),
            profile: "disabled".to_string(),
            model: None,
            effort: None,
        }],
        None,
    )
    .await;
    assert!(
        matches!(reply, DaemonMessage::RoleList { .. }),
        "a role naming a disabled (but listed) profile must save: {reply:?}"
    );
}

#[tokio::test]
async fn saving_a_role_table_naming_a_profile_with_no_argv_for_this_operating_system_is_refused() {
    let dir = unique_temp_dir("profile-enabled-role-save-no-os");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_profile(
        &profiles_dir,
        "no-host-argv",
        &profile_source_with_no_argv_for_host("No Host Argv"),
    );
    let roles_path = dir.join("roles.toml");
    let (h, _locks) = Harness::start_with_ledger(
        "profile-enabled-role-save-no-os",
        ledger_config(&dir, profiles_dir, Some(roles_path)),
    )
    .await;

    let mut approver = approver_connection(&h).await;
    let reply = save_roles(
        &mut approver,
        vec![RoleEntry {
            name: "reviewer".to_string(),
            profile: "no-host-argv".to_string(),
            model: None,
            effort: None,
        }],
        None,
    )
    .await;
    let message = expect_error(reply, "invalid_argument");
    assert_eq!(message, RolesError::UnknownProfile.to_string());
}

#[tokio::test]
async fn create_plan_accepts_a_task_naming_a_disabled_profile() {
    let dir = unique_temp_dir("profile-enabled-create-plan");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_profile(
        &profiles_dir,
        "disabled",
        &echo_child_profile_source("Disabled", Some(false)),
    );
    let (h, _locks) = Harness::start_with_ledger(
        "profile-enabled-create-plan",
        ledger_config(&dir, profiles_dir, None),
    )
    .await;

    let mut owner = h.connect().await;
    let owner_window = h
        .open(yagura_test_support::process::echo_child_command(), None)
        .await;
    identify(&mut owner, &owner_window).await;
    let plan = create_plan(
        &mut owner,
        "plan",
        vec![PlanTaskSpec {
            task_id: "t1".to_string(),
            title: "do the thing".to_string(),
            role: None,
            profile: Some("disabled".to_string()),
            model: None,
            effort: None,
            depends_on: Vec::new(),
            writes: vec!["t1.txt".to_string()],
            instruction: None,
        }],
    )
    .await;
    assert_eq!(plan.tasks.len(), 1);
}

// =============================================================================
// list_model_choices for a disabled profile never starts the
// listing command, with an enabled control that does.
// =============================================================================

fn capture_child_path() -> PathBuf {
    let mut dir =
        std::env::current_exe().expect("current_exe should resolve for a running test binary");
    dir.pop();
    if dir.file_name().and_then(|name| name.to_str()) == Some("deps") {
        dir.pop();
    }
    let name = if cfg!(windows) {
        "capture_child.exe"
    } else {
        "capture_child"
    };
    dir.push(name);
    assert!(
        dir.exists(),
        "the capture_child fixture binary is missing at {}: run `cargo build -p yagura-daemon \
         --bin capture_child` first",
        dir.display()
    );
    dir
}

/// A profile whose `[model.choices]` command touches `marker_path` at
/// startup -- proof the listing command actually started -- switched off
/// when `enabled` is `false`.
fn touch_profile_source(display_name: &str, marker_path: &Path, enabled: Option<bool>) -> String {
    let enabled_line = match enabled {
        Some(value) => format!("enabled = {value}\n"),
        None => String::new(),
    };
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\n{enabled_line}\n[command]\ndefault = \
         ['{}']\n\n[model]\nargs = [\"--tier\", \"{{value}}\"]\n\n[model.choices]\nargs = \
         [\"touch\", '{}', \"exit\", \"0\"]\nformat = \"lines\"\nseparator = \"\\t\"\n",
        capture_child_path().display(),
        marker_path.display(),
    )
}

async fn wait_until_marker_appears(marker: &Path, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if marker.exists() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for the marker file to appear at {}",
                marker.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// How long a case that must observe "no listing command started" waits
/// before checking, the same budget `ipc_model_aliases.rs`'s own
/// `NEGATIVE_OBSERVATION` uses.
const NEGATIVE_OBSERVATION: Duration = Duration::from_millis(500);

async fn assert_marker_absent_after(marker: &Path, observation: Duration) {
    tokio::time::sleep(observation).await;
    assert!(
        !marker.exists(),
        "no listing command must have started: marker file exists at {}",
        marker.display()
    );
}

async fn call_list_model_choices(
    client: &mut Connection,
    profile: &str,
    request_id: u64,
) -> (Option<ModelChoiceSource>, Vec<ModelChoice>, Option<String>) {
    send(
        client,
        &ClientMessage::ListModelChoices {
            profile: profile.to_string(),
            request_id,
        },
    )
    .await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::ModelChoicesRequested { .. } => {}
        other => panic!("expected model_choices_requested, got {other:?}"),
    }
    match read_response(client, Duration::from_secs(10)).await {
        DaemonMessage::ModelChoices {
            source,
            choices,
            error,
            ..
        } => (source, choices, error),
        other => panic!("expected model_choices, got {other:?}"),
    }
}

#[tokio::test]
async fn list_model_choices_for_a_disabled_profile_reports_unavailable_and_starts_no_listing_command()
 {
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-model-choices");
    let profiles_dir = tree.make_dir("profiles");
    let marker = tree.make_dir("marker-parent").join("marker.txt");
    let enabled_marker = tree.make_dir("enabled-marker-parent").join("marker.txt");
    write_profile(
        &profiles_dir,
        "disabled",
        &touch_profile_source("Disabled", &marker, Some(false)),
    );
    write_profile(
        &profiles_dir,
        "enabled",
        &touch_profile_source("Enabled", &enabled_marker, Some(true)),
    );

    let config = DaemonConfig {
        profiles_dir: Some(profiles_dir),
        model_choices_max_concurrent: 2,
        model_choices_timeout: Duration::from_secs(10),
        model_choices_max_output_bytes: 1 << 20,
        ..approver_base_config()
    };
    let h = Harness::start("profile-enabled-model-choices", config).await;
    let mut approver = approver_connection(&h).await;

    let (source, choices, error) = call_list_model_choices(&mut approver, "disabled", 1).await;
    assert!(source.is_none());
    assert!(choices.is_empty());
    let message = error.expect("a disabled profile must report an error");
    assert_eq!(
        message,
        yagura_daemon::profile::ProfileLaunchError::Disabled.to_string(),
        "the reported reason must be the same fixed sentence resolve_launch uses for a \
         disabled profile"
    );

    assert_marker_absent_after(&marker, NEGATIVE_OBSERVATION).await;

    // Control: the same shape, enabled, actually starts the listing command
    // (the parsed result is not this test's concern -- `touch`'s own empty
    // stdout need not match `format = "lines"`; only that the command ran).
    let (source, ..) = call_list_model_choices(&mut approver, "enabled", 2).await;
    assert!(source.is_some());
    wait_until_marker_appears(&enabled_marker, Duration::from_secs(10)).await;
}

// =============================================================================
// SetProfileEnabled itself -- authorization, unsupported /
// invalid_argument, and a successful switch that later listings and launches
// follow.
// =============================================================================

#[tokio::test]
async fn set_profile_enabled_from_a_window_identified_connection_is_auth_failed_and_leaves_the_file_unchanged()
 {
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-set-window-auth");
    let profiles_dir = tree.make_dir("profiles");
    let path = profiles_dir.join("target.toml");
    write_profile(
        &profiles_dir,
        "target",
        &echo_child_profile_source("Target", None),
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("profile-enabled-set-window-auth", config).await;
    let mut client = harness.connect().await;
    let window_id =
        expect_window_opened(call_open_window_from_profile(&mut client, "target").await);
    identify(&mut client, &window_id).await;

    let before = std::fs::read_to_string(&path).expect("profile file must be readable");
    let reply = call_set_profile_enabled(&mut client, "target", false).await;
    expect_error(reply, "auth_failed");
    let after = std::fs::read_to_string(&path).expect("profile file must be readable");
    assert_eq!(
        before, after,
        "a refused write must leave the file byte-for-byte unchanged"
    );

    harness.registry.terminate_all().await;
}

#[tokio::test]
async fn set_profile_enabled_from_an_unidentified_connection_is_auth_failed() {
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-set-external-auth");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "target",
        &echo_child_profile_source("Target", None),
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("profile-enabled-set-external-auth", config).await;
    let mut client = harness.connect().await;

    let reply = call_set_profile_enabled(&mut client, "target", false).await;
    expect_error(reply, "auth_failed");
}

#[tokio::test]
async fn set_profile_enabled_with_no_profiles_directory_is_unsupported() {
    let config = config_with(None, empty_lookup());
    let harness = Harness::start("profile-enabled-set-no-dir", config).await;
    let mut approver = approver_connection(&harness).await;

    let reply = call_set_profile_enabled(&mut approver, "whatever", false).await;
    expect_error(reply, "unsupported");
}

#[tokio::test]
async fn set_profile_enabled_with_an_unknown_profile_id_is_invalid_argument() {
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-set-unknown");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "the-only-profile",
        &echo_child_profile_source("Only", None),
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("profile-enabled-set-unknown", config).await;
    let mut approver = approver_connection(&harness).await;

    let reply = call_set_profile_enabled(&mut approver, "does-not-exist-anywhere", false).await;
    expect_error(reply, "invalid_argument");
}

#[tokio::test]
async fn set_profile_enabled_by_an_approver_switches_the_profile_and_later_listings_and_launches_follow()
 {
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-set-success");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "target",
        &echo_child_profile_source("Target", None),
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("profile-enabled-set-success", config).await;
    let mut approver = approver_connection(&harness).await;

    // Off: the request answers with the stored value, the listing reflects
    // it, and a launch is now refused.
    let reply = call_set_profile_enabled(&mut approver, "target", false).await;
    match reply {
        DaemonMessage::ProfileEnabledSet { profile, enabled } => {
            assert_eq!(profile, "target");
            assert!(!enabled);
        }
        other => panic!("expected profile_enabled_set, got {other:?}"),
    }
    let profiles = call_list_profiles(&mut approver).await;
    assert!(!find_profile(&profiles, "target").enabled);
    let response = call_open_window_from_profile(&mut approver, "target").await;
    expect_error(response, "profile_disabled");

    // On: the request answers with the stored value, the listing reflects
    // it, and a launch now succeeds.
    let reply = call_set_profile_enabled(&mut approver, "target", true).await;
    match reply {
        DaemonMessage::ProfileEnabledSet { profile, enabled } => {
            assert_eq!(profile, "target");
            assert!(enabled);
        }
        other => panic!("expected profile_enabled_set, got {other:?}"),
    }
    let profiles = call_list_profiles(&mut approver).await;
    assert!(find_profile(&profiles, "target").enabled);
    let response = call_open_window_from_profile(&mut approver, "target").await;
    expect_window_opened(response);

    harness.registry.terminate_all().await;
}
