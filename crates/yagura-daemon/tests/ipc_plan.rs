#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the plan ledger's IPC surface (`create_plan` /
//! `update_plan` / `get_plan` / `list_plans` / `delete_plan`), driven
//! through `Harness::start_with_ledger` -- a real `WindowRegistry` +
//! `IpcServer` pair with a real SQLite-backed plan store and the shared
//! per-plan lock table wired in.
//!
//! `open_task_window`, the role table and the assignment-override tools are
//! out of scope here: no task is ever bound to a window in this file, so a
//! plan is always read through its owner or an approver, never through a
//! bound window. Every task fixture leaves `role` / `profile` unset, so
//! assignment resolution always answers "unassigned" and the role table is
//! never exercised. Children are `echo_child` or a fixed self-exiting
//! command, never a real vendor CLI.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{
    Harness, approver_base_config, expect_error, identify, policy, read_response,
    register_approver, send, unique_temp_dir, write_window_input,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::plan;
use yagura_daemon::plan_permit::LockTable;
use yagura_protocol::{ClientMessage, DaemonMessage, PlanInfo, PlanSummary, PlanTaskSpec};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{echo_child_path, exit_with_code_command};

// --- fixtures ----------------------------------------------------------

fn echo_command() -> Vec<String> {
    let child = echo_child_path();
    vec![
        child
            .to_str()
            .expect("echo_child path must be valid UTF-8")
            .to_string(),
    ]
}

fn self_exiting_command() -> Vec<String> {
    exit_with_code_command("0")
}

/// A child that stays alive until one line arrives on its input and then
/// exits on its own, so a test decides when the owner's child ends.
#[cfg(windows)]
fn exits_after_one_line_command() -> Vec<String> {
    ["cmd.exe", "/c", "set", "/p", "line="]
        .map(str::to_string)
        .to_vec()
}

/// Unix counterpart of the Windows `exits_after_one_line_command`.
#[cfg(not(windows))]
fn exits_after_one_line_command() -> Vec<String> {
    ["/bin/sh", "-c", "read line; exit 0"]
        .map(str::to_string)
        .to_vec()
}

/// A task with no role/profile (always resolves "unassigned") and one
/// write target unique to `task_id`, so two independently-built tasks never
/// collide unless a case deliberately reuses a path.
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

fn tasks(task_ids: &[&str]) -> Vec<PlanTaskSpec> {
    task_ids.iter().map(|id| task(id)).collect()
}

/// A [`DaemonConfig`] pointed at a fresh plan store under `dir`, with the
/// approver identity every case needs to register as an approver, and
/// `auto_approve` set to whatever destructive tools (e.g. `close_window`)
/// the case needs to drive without a human decision.
fn ledger_config(dir: &Path, auto_approve: &[&str]) -> DaemonConfig {
    let mut config = approver_base_config();
    config.approval_policy = Some(policy(auto_approve));
    config.store_db_path_resolved = Some(dir.join("plan.db"));
    config
}

async fn start_ledger(tag: &str, config: DaemonConfig) -> (Harness, Arc<LockTable>) {
    Harness::start_with_ledger(tag, config).await
}

/// Opens an `echo_child` window and identifies `client` as it, returning its
/// window id.
async fn open_and_identify(h: &Harness, client: &mut Connection) -> String {
    let window_id = h.open(echo_command(), None).await;
    identify(client, &window_id).await;
    window_id
}

async fn approver_connection(h: &Harness) -> Connection {
    let mut client = h.connect().await;
    register_approver(&mut client).await;
    client
}

/// Fails a request that never gets its reply. Not a speed bound: a plan at
/// every cap takes seconds to check in a debug build on a slow runner.
const PLAN_REPLY_GUARD: Duration = Duration::from_secs(60);

/// Reads the next *response* frame, skipping `plan_changed` pushes the same
/// way `read_response` already skips `Output` / `StatusChanged`: a
/// connection that is also a registered approver may be pushed to while its
/// own request is in flight.
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
    read_plan_reply(client, PLAN_REPLY_GUARD).await
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
    read_plan_reply(client, PLAN_REPLY_GUARD).await
}

async fn get_plan(client: &mut Connection, plan_id: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::GetPlan {
            plan_id: plan_id.to_string(),
        },
    )
    .await;
    read_plan_reply(client, PLAN_REPLY_GUARD).await
}

async fn list_plans(client: &mut Connection) -> DaemonMessage {
    send(client, &ClientMessage::ListPlans).await;
    read_plan_reply(client, PLAN_REPLY_GUARD).await
}

async fn delete_plan(client: &mut Connection, plan_id: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::DeletePlan {
            plan_id: plan_id.to_string(),
        },
    )
    .await;
    read_plan_reply(client, PLAN_REPLY_GUARD).await
}

fn expect_plan_detail(msg: DaemonMessage) -> PlanInfo {
    match msg {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

fn expect_plan_list(msg: DaemonMessage) -> Vec<PlanSummary> {
    match msg {
        DaemonMessage::PlanList { plans } => plans,
        other => panic!("expected plan_list, got {other:?}"),
    }
}

fn expect_plan_deleted(msg: DaemonMessage) -> String {
    match msg {
        DaemonMessage::PlanDeleted { plan_id } => plan_id,
        other => panic!("expected plan_deleted, got {other:?}"),
    }
}

async fn create_ok(
    client: &mut Connection,
    title: &str,
    plan_tasks: Vec<PlanTaskSpec>,
) -> PlanInfo {
    expect_plan_detail(create_plan(client, title, plan_tasks).await)
}

// Any identified window can create its own plan.

#[tokio::test]
async fn create_plan_makes_the_calling_window_its_owner_with_the_windows_cwd() {
    let dir = unique_temp_dir("ipc-plan-create-owner");
    let (h, _locks) = start_ledger("ipc-plan-create-owner", ledger_config(&dir, &[])).await;

    let workspace = unique_temp_dir("ipc-plan-create-owner-cwd");
    let window_a = h.open_in_cwd(echo_command(), None, &workspace).await;
    let mut client_a = h.connect().await;
    identify(&mut client_a, &window_a).await;

    let plan = create_ok(&mut client_a, "plan a", tasks(&["t1"])).await;
    assert_eq!(
        plan.owner_window.as_deref(),
        Some(window_a.as_str()),
        "the window that sent create_plan must become the plan's owner"
    );
    let expected_cwd = yagura_test_support::paths::strip_extended_length_prefix(
        &workspace
            .canonicalize()
            .expect("workspace must canonicalize"),
    )
    .to_string_lossy()
    .into_owned();
    assert_eq!(
        plan.cwd, expected_cwd,
        "the plan's cwd must be the creating window's own cwd"
    );

    // A second, unrelated window creates its own plan and becomes its own
    // owner: creation is not limited to one plan-creating window per daemon.
    let mut client_b = h.connect().await;
    let window_b = open_and_identify(&h, &mut client_b).await;
    let plan_b = create_ok(&mut client_b, "plan b", tasks(&["t1"])).await;
    assert_eq!(plan_b.owner_window.as_deref(), Some(window_b.as_str()));
    assert_ne!(plan.plan_id, plan_b.plan_id);
}

// External is always auth_failed; only a window may create a plan.

#[tokio::test]
async fn external_is_auth_failed_and_an_approver_cannot_create_a_plan() {
    let dir = unique_temp_dir("ipc-plan-external-approver");
    let (h, _locks) = start_ledger("ipc-plan-external-approver", ledger_config(&dir, &[])).await;

    let mut external = h.connect().await;
    let reply = create_plan(&mut external, "plan", tasks(&["t1"])).await;
    expect_error(reply, "auth_failed");
    let reply = get_plan(&mut external, "does-not-exist").await;
    expect_error(reply, "auth_failed");
    let reply = list_plans(&mut external).await;
    expect_error(reply, "auth_failed");

    let mut approver = approver_connection(&h).await;
    let reply = create_plan(&mut approver, "plan", tasks(&["t1"])).await;
    expect_error(reply, "auth_failed");
}

// update_plan is scoped to the alive owner.

#[tokio::test]
async fn update_plan_is_authorized_only_for_the_alive_owner() {
    let dir = unique_temp_dir("ipc-plan-update-scope");
    let (h, _locks) = start_ledger("ipc-plan-update-scope", ledger_config(&dir, &[])).await;

    let mut owner = h.connect().await;
    let _owner_window = open_and_identify(&h, &mut owner).await;
    let plan = create_ok(&mut owner, "plan", tasks(&["t1"])).await;

    let updated =
        expect_plan_detail(update_plan(&mut owner, &plan.plan_id, tasks(&["t1", "t2"])).await);
    assert_eq!(
        updated.tasks.len(),
        2,
        "the owner's update_plan must take effect"
    );

    let mut other = h.connect().await;
    open_and_identify(&h, &mut other).await;
    let reply = update_plan(&mut other, &plan.plan_id, tasks(&["t1"])).await;
    expect_error(reply, "plan_not_found");

    let mut approver = approver_connection(&h).await;
    let reply = update_plan(&mut approver, &plan.plan_id, tasks(&["t1"])).await;
    expect_error(reply, "not_plan_owner");
}

// The read range follows ownership; out-of-range windows are excluded.

#[tokio::test]
async fn read_range_is_the_owner_and_the_approver_out_of_range_windows_are_excluded() {
    let dir = unique_temp_dir("ipc-plan-read-range");
    let (h, _locks) = start_ledger("ipc-plan-read-range", ledger_config(&dir, &[])).await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(&mut owner, "plan", tasks(&["t1"])).await;

    let read_by_owner = expect_plan_detail(get_plan(&mut owner, &plan.plan_id).await);
    assert_eq!(read_by_owner.plan_id, plan.plan_id);
    let owner_list = expect_plan_list(list_plans(&mut owner).await);
    assert!(owner_list.iter().any(|p| p.plan_id == plan.plan_id));

    let mut other = h.connect().await;
    open_and_identify(&h, &mut other).await;
    let reply = get_plan(&mut other, &plan.plan_id).await;
    expect_error(reply, "plan_not_found");
    let other_list = expect_plan_list(list_plans(&mut other).await);
    assert!(
        !other_list.iter().any(|p| p.plan_id == plan.plan_id),
        "list_plans must not name a plan the calling window has no relation to"
    );

    let mut approver = approver_connection(&h).await;
    let read_by_approver = expect_plan_detail(get_plan(&mut approver, &plan.plan_id).await);
    assert_eq!(read_by_approver.plan_id, plan.plan_id);
    let approver_list = expect_plan_list(list_plans(&mut approver).await);
    assert!(approver_list.iter().any(|p| p.plan_id == plan.plan_id));
}

// delete_plan admits the alive owner and the approver.

#[tokio::test]
async fn delete_plan_admits_the_alive_owner_and_the_approver_but_not_other_windows() {
    let dir = unique_temp_dir("ipc-plan-delete-scope");
    let (h, _locks) = start_ledger("ipc-plan-delete-scope", ledger_config(&dir, &[])).await;

    let mut owner_a = h.connect().await;
    open_and_identify(&h, &mut owner_a).await;
    let plan_a = create_ok(&mut owner_a, "plan a", tasks(&["t1"])).await;

    let mut other = h.connect().await;
    open_and_identify(&h, &mut other).await;
    let reply = delete_plan(&mut other, &plan_a.plan_id).await;
    expect_error(reply, "plan_not_found");

    let deleted = expect_plan_deleted(delete_plan(&mut owner_a, &plan_a.plan_id).await);
    assert_eq!(deleted, plan_a.plan_id);
    let reply = get_plan(&mut owner_a, &plan_a.plan_id).await;
    expect_error(reply, "plan_not_found");

    let mut owner_b = h.connect().await;
    open_and_identify(&h, &mut owner_b).await;
    let plan_b = create_ok(&mut owner_b, "plan b", tasks(&["t1"])).await;
    let mut approver = approver_connection(&h).await;
    let deleted = expect_plan_deleted(delete_plan(&mut approver, &plan_b.plan_id).await);
    assert_eq!(deleted, plan_b.plan_id);
}

// An out-of-range plan_id and a nonexistent one are indistinguishable.

#[tokio::test]
async fn out_of_range_and_nonexistent_plan_ids_produce_byte_identical_refusals() {
    let dir = unique_temp_dir("ipc-plan-indistinguishable");
    let (h, _locks) = start_ledger("ipc-plan-indistinguishable", ledger_config(&dir, &[])).await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(&mut owner, "plan", tasks(&["t1"])).await;

    let mut other = h.connect().await;
    open_and_identify(&h, &mut other).await;

    for (real_reply, absent_reply) in [
        (
            get_plan(&mut other, &plan.plan_id).await,
            get_plan(&mut other, "plan-does-not-exist-at-all").await,
        ),
        (
            update_plan(&mut other, &plan.plan_id, tasks(&["t1"])).await,
            update_plan(&mut other, "plan-does-not-exist-at-all", tasks(&["t1"])).await,
        ),
        (
            delete_plan(&mut other, &plan.plan_id).await,
            delete_plan(&mut other, "plan-does-not-exist-at-all").await,
        ),
    ] {
        let real_message = expect_error(real_reply, "plan_not_found");
        let absent_message = expect_error(absent_reply, "plan_not_found");
        assert_eq!(
            real_message, absent_message,
            "a plan the caller is out of range of and a plan_id that does not exist at all \
             must produce byte-identical refusals"
        );
    }
}

// Validation failures are invalid_argument, and rejected values never leak.

#[tokio::test]
async fn create_plan_validation_failures_are_invalid_argument_and_never_echo_the_rejected_value() {
    let dir = unique_temp_dir("ipc-plan-validation");
    let (h, _locks) = start_ledger("ipc-plan-validation", ledger_config(&dir, &[])).await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;

    // Empty title: a title-shaped violation.
    let reply = create_plan(&mut owner, "", tasks(&["t1"])).await;
    let (code, argument) = expect_error_with_argument(reply);
    assert_eq!(code, "invalid_argument");
    assert_eq!(argument.as_deref(), Some("title"));

    // No tasks at all: a tasks-shaped violation.
    let reply = create_plan(&mut owner, "plan", Vec::new()).await;
    let (code, argument) = expect_error_with_argument(reply);
    assert_eq!(code, "invalid_argument");
    assert_eq!(argument.as_deref(), Some("tasks"));

    // A write-target conflict between two independent tasks: both task ids
    // and the colliding path must appear in the message, and it must never
    // echo a value that failed shape validation outright.
    let mut conflicting = tasks(&["alpha", "beta"]);
    for t in &mut conflicting {
        t.writes = vec!["shared/output.txt".to_string()];
    }
    let reply = create_plan(&mut owner, "plan", conflicting).await;
    let message = expect_error(reply, "invalid_argument");
    assert!(
        message.contains("alpha")
            && message.contains("beta")
            && message.contains("shared/output.txt"),
        "a write-target conflict must report every colliding task id and path, got {message:?}"
    );

    // A write-target path carrying a forbidden character (a control
    // character) is rejected, and that raw value never appears in the
    // message.
    let mut poisoned = tasks(&["t1"]);
    poisoned[0].writes = vec!["poison-marker-\u{7}-value".to_string()];
    let reply = create_plan(&mut owner, "plan", poisoned).await;
    let message = expect_error(reply, "invalid_argument");
    assert!(
        !message.contains("poison-marker"),
        "a rejected value must never be echoed back in the message, got {message:?}"
    );
}

fn expect_error_with_argument(msg: DaemonMessage) -> (String, Option<String>) {
    match msg {
        DaemonMessage::Error {
            code,
            rejected_argument,
            ..
        } => (code, rejected_argument),
        other => panic!("expected error, got {other:?}"),
    }
}

// Plan creation is capped per owner and overall.

#[tokio::test]
async fn plan_creation_is_capped_per_owner_and_overall() {
    let dir = unique_temp_dir("ipc-plan-caps");
    let (h, _locks) = start_ledger("ipc-plan-caps", ledger_config(&dir, &[])).await;

    let mut window_a = h.connect().await;
    open_and_identify(&h, &mut window_a).await;

    for i in 0..plan_store_max_plans_per_owner() {
        let reply = create_plan(&mut window_a, "plan", tasks(&[&format!("t{i}")])).await;
        expect_plan_detail(reply);
    }
    let reply = create_plan(&mut window_a, "plan", tasks(&["overflow"])).await;
    expect_error(reply, "resource_exhausted");

    // A different window's own per-owner budget is untouched.
    let mut window_b = h.connect().await;
    open_and_identify(&h, &mut window_b).await;
    let reply = create_plan(&mut window_b, "plan", tasks(&["t0"])).await;
    expect_plan_detail(reply);

    // Fill the remaining per-owner budget for three more windows so the
    // overall cap is reached without any single window exceeding its own.
    for _ in 0..(plan_store_max_plans_per_owner() - 1) {
        let reply = create_plan(
            &mut window_b,
            "plan",
            tasks(&[&format!("t{}", unique_counter())]),
        )
        .await;
        expect_plan_detail(reply);
    }
    let mut window_c = h.connect().await;
    open_and_identify(&h, &mut window_c).await;
    for _ in 0..plan_store_max_plans_per_owner() {
        let reply = create_plan(
            &mut window_c,
            "plan",
            tasks(&[&format!("t{}", unique_counter())]),
        )
        .await;
        expect_plan_detail(reply);
    }
    let mut window_d = h.connect().await;
    open_and_identify(&h, &mut window_d).await;
    for _ in 0..plan_store_max_plans_per_owner() {
        let reply = create_plan(
            &mut window_d,
            "plan",
            tasks(&[&format!("t{}", unique_counter())]),
        )
        .await;
        expect_plan_detail(reply);
    }

    // The store now holds MAX_TOTAL_PLANS plans (16 each across A..D). A
    // fresh window, with zero plans of its own, must still be refused by
    // the overall cap.
    let mut window_e = h.connect().await;
    open_and_identify(&h, &mut window_e).await;
    let reply = create_plan(&mut window_e, "plan", tasks(&["t0"])).await;
    expect_error(reply, "resource_exhausted");
}

fn plan_store_max_plans_per_owner() -> usize {
    usize::try_from(yagura_daemon::plan_store::MAX_PLANS_PER_OWNER)
        .expect("MAX_PLANS_PER_OWNER must fit in usize")
}

fn unique_counter() -> u64 {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

// Every mutation pushes plan_changed to every approver.

#[tokio::test]
async fn every_ledger_mutation_pushes_plan_changed_to_every_registered_approver() {
    let dir = unique_temp_dir("ipc-plan-push");
    let (h, _locks) = start_ledger("ipc-plan-push", ledger_config(&dir, &[])).await;

    let mut approver = approver_connection(&h).await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(&mut owner, "plan", tasks(&["t1"])).await;
    expect_plan_changed(&mut approver, &plan.plan_id).await;

    expect_plan_detail(update_plan(&mut owner, &plan.plan_id, tasks(&["t1", "t2"])).await);
    expect_plan_changed(&mut approver, &plan.plan_id).await;

    expect_plan_deleted(delete_plan(&mut owner, &plan.plan_id).await);
    expect_plan_changed(&mut approver, &plan.plan_id).await;
}

async fn expect_plan_changed(client: &mut Connection, plan_id: &str) {
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::PlanChanged { plan_id: pushed } => assert_eq!(pushed, plan_id),
        other => panic!("expected plan_changed, got {other:?}"),
    }
}

// Owner authorization requires the owner window to be alive.

#[tokio::test]
async fn owner_authorization_is_refused_once_the_owner_window_is_closed() {
    let dir = unique_temp_dir("ipc-plan-closed-owner");
    let (h, _locks) = start_ledger(
        "ipc-plan-closed-owner",
        ledger_config(&dir, &["close_window"]),
    )
    .await;

    let mut owner = h.connect().await;
    let owner_window = open_and_identify(&h, &mut owner).await;
    let plan = create_ok(&mut owner, "plan", tasks(&["t1"])).await;

    let mut closer = approver_connection(&h).await;
    send(
        &mut closer,
        &ClientMessage::CloseWindow {
            window_id: owner_window.clone(),
            force: None,
        },
    )
    .await;
    match read_response(&mut closer, Duration::from_secs(10)).await {
        DaemonMessage::WindowClosed { window_id } => assert_eq!(window_id, owner_window),
        other => panic!("expected window_closed, got {other:?}"),
    }

    // The now-closed owner's own connection is still identified; it must no
    // longer be authorized to change the plan's structure.
    let reply = update_plan(&mut owner, &plan.plan_id, tasks(&["t1"])).await;
    expect_error(reply, "not_plan_owner");
    let reply = delete_plan(&mut owner, &plan.plan_id).await;
    expect_error(reply, "not_plan_owner");

    // The stored owner id is unchanged -- daemon-side authorization judged
    // liveness, it did not clear the owner field.
    let read_by_approver = expect_plan_detail(get_plan(&mut closer, &plan.plan_id).await);
    assert_eq!(
        read_by_approver.owner_window.as_deref(),
        Some(owner_window.as_str())
    );
}

#[tokio::test]
async fn owner_authorization_is_refused_once_the_owner_windows_child_has_exited() {
    let dir = unique_temp_dir("ipc-plan-exited-owner");
    let (h, _locks) = start_ledger("ipc-plan-exited-owner", ledger_config(&dir, &[])).await;

    let owner_window = h.open(exits_after_one_line_command(), None).await;
    let mut owner = h.connect().await;
    identify(&mut owner, &owner_window).await;
    let plan = create_ok(&mut owner, "plan", tasks(&["t1"])).await;

    write_window_input(&h.registry, &owner_window, b"\r").await;
    h.wait_until_child_exited(&owner_window, Duration::from_secs(10))
        .await;

    let reply = update_plan(&mut owner, &plan.plan_id, tasks(&["t1"])).await;
    expect_error(reply, "not_plan_owner");

    let mut approver = approver_connection(&h).await;
    let read_by_approver = expect_plan_detail(get_plan(&mut approver, &plan.plan_id).await);
    assert_eq!(
        read_by_approver.owner_window.as_deref(),
        Some(owner_window.as_str())
    );
}

// A restart against the same DB keeps plans and clears owners.

#[tokio::test]
async fn plans_survive_a_restart_against_the_same_db_but_owners_are_cleared() {
    let dir = unique_temp_dir("ipc-plan-restart");
    let config = ledger_config(&dir, &[]);

    let plan_id;
    {
        let (h, _locks) = start_ledger("ipc-plan-restart-a", config.clone()).await;
        let mut owner = h.connect().await;
        open_and_identify(&h, &mut owner).await;
        let plan = create_ok(&mut owner, "plan", tasks(&["t1"])).await;
        plan_id = plan.plan_id;
    }

    let (h, _locks) = start_ledger("ipc-plan-restart-b", config).await;
    let mut approver = approver_connection(&h).await;
    let plan = expect_plan_detail(get_plan(&mut approver, &plan_id).await);
    assert_eq!(plan.plan_id, plan_id);
    assert_eq!(plan.tasks.len(), 1);
    assert_eq!(
        plan.owner_window, None,
        "a restart must clear every plan's owner, since window ids from the previous boot \
         cannot survive it"
    );
}

// The store-less minimal arm, and a real rejection on a store-backed daemon.

#[tokio::test]
async fn plan_tools_reply_unsupported_and_keep_the_connection_alive_without_a_store() {
    let h = Harness::start("ipc-plan-no-store", approver_base_config()).await;
    let mut client = h.connect().await;
    let _window_id = open_and_identify(&h, &mut client).await;

    let reply = create_plan(&mut client, "plan", tasks(&["t1"])).await;
    expect_error(reply, "unsupported");

    // The connection must still be usable after the refusal.
    send(&mut client, &ClientMessage::Ping).await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::Pong => {}
        other => panic!("expected pong after an unsupported plan tool, got {other:?}"),
    }
}

#[tokio::test]
async fn a_rejected_request_keeps_the_connection_alive_on_a_ledger_backed_daemon() {
    let dir = unique_temp_dir("ipc-plan-rejection-alive");
    let (h, _locks) = start_ledger("ipc-plan-rejection-alive", ledger_config(&dir, &[])).await;

    let mut external = h.connect().await;
    let reply = create_plan(&mut external, "plan", tasks(&["t1"])).await;
    expect_error(reply, "auth_failed");

    send(&mut external, &ClientMessage::Ping).await;
    match read_response(&mut external, Duration::from_secs(5)).await {
        DaemonMessage::Pong => {}
        other => panic!("expected pong after a real ledger refusal, got {other:?}"),
    }
}

// A plan at every declared cap round-trips in one frame.

fn max_write_paths() -> Vec<String> {
    (0..plan::MAX_TASK_WRITES)
        .map(|i| {
            let prefix = format!("w{i:02}/");
            let filler_len = plan::MAX_WRITE_PATH_BYTES - prefix.len();
            format!("{prefix}{}", "x".repeat(filler_len))
        })
        .collect()
}

fn max_size_tasks() -> Vec<PlanTaskSpec> {
    let writes = max_write_paths();
    let title = "t".repeat(120);
    (0..plan::MAX_PLAN_TASKS)
        .map(|i| {
            let task_id = format!("t{i}");
            let depends_on = (0..i).map(|j| format!("t{j}")).collect::<Vec<_>>();
            PlanTaskSpec {
                task_id,
                title: title.clone(),
                role: None,
                profile: None,
                model: None,
                effort: None,
                depends_on,
                writes: writes.clone(),
                instruction: None,
            }
        })
        .collect()
}

#[tokio::test]
async fn a_plan_at_every_declared_cap_round_trips_in_one_frame() {
    let dir = unique_temp_dir("ipc-plan-max-size");
    let (h, _locks) = start_ledger("ipc-plan-max-size", ledger_config(&dir, &[])).await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;

    let plan = create_ok(&mut owner, "max size plan", max_size_tasks()).await;
    assert_eq!(plan.tasks.len(), plan::MAX_PLAN_TASKS);
    assert_eq!(
        plan.waves.len(),
        plan::MAX_PLAN_TASKS,
        "a full dependency chain must produce one task per wave"
    );
    assert_eq!(plan.waves[0], vec!["t0".to_string()]);
    assert_eq!(
        plan.waves[plan::MAX_PLAN_TASKS - 1],
        vec![format!("t{}", plan::MAX_PLAN_TASKS - 1)]
    );

    let encoded =
        yagura_protocol::encode_daemon_message(&DaemonMessage::PlanDetail { plan: plan.clone() });
    assert!(
        encoded.len() <= yagura_protocol::MAX_FRAME_BODY_LEN,
        "the encoded plan_detail body must fit the frame budget, got {} bytes",
        encoded.len()
    );
}

// create_plan is refused for a window that is not alive.

#[tokio::test]
async fn create_plan_is_refused_for_a_window_that_is_not_alive() {
    let dir = unique_temp_dir("ipc-plan-dead-creator");
    let (h, _locks) = start_ledger(
        "ipc-plan-dead-creator",
        ledger_config(&dir, &["close_window"]),
    )
    .await;

    let mut closed_owner = h.connect().await;
    let closed_window = open_and_identify(&h, &mut closed_owner).await;
    let before = expect_plan_list(list_plans(&mut approver_connection(&h).await).await).len();

    let mut closer = approver_connection(&h).await;
    send(
        &mut closer,
        &ClientMessage::CloseWindow {
            window_id: closed_window.clone(),
            force: None,
        },
    )
    .await;
    match read_response(&mut closer, Duration::from_secs(10)).await {
        DaemonMessage::WindowClosed { .. } => {}
        other => panic!("expected window_closed, got {other:?}"),
    }
    let reply = create_plan(&mut closed_owner, "plan", tasks(&["t1"])).await;
    expect_error(reply, "invalid_state");

    let exited_window = h.open(self_exiting_command(), None).await;
    let mut exited_owner = h.connect().await;
    identify(&mut exited_owner, &exited_window).await;
    h.wait_until_child_exited(&exited_window, Duration::from_secs(10))
        .await;
    let reply = create_plan(&mut exited_owner, "plan", tasks(&["t1"])).await;
    expect_error(reply, "invalid_state");

    let after = expect_plan_list(list_plans(&mut approver_connection(&h).await).await).len();
    assert_eq!(
        before, after,
        "a refused creation from a window that is not alive must not add a row"
    );
}

// A held plan lock delays a conflicting change, and the authorization
// grant is re-checked once the lock is finally taken.

#[tokio::test]
async fn a_held_plan_lock_delays_a_conflicting_update_and_the_grant_is_re_checked_inside_it() {
    let dir = unique_temp_dir("ipc-plan-lock-delay");
    let (h, locks) = start_ledger(
        "ipc-plan-lock-delay",
        ledger_config(&dir, &["close_window"]),
    )
    .await;

    let mut owner = h.connect().await;
    let owner_window = open_and_identify(&h, &mut owner).await;
    let plan = create_ok(&mut owner, "plan", tasks(&["t1"])).await;

    let held = locks.lock(&plan.plan_id).await;

    send(
        &mut owner,
        &ClientMessage::UpdatePlan {
            plan_id: plan.plan_id.clone(),
            title: None,
            tasks: tasks(&["t1", "t2"]),
        },
    )
    .await;
    let blocked = tokio::time::timeout(
        Duration::from_millis(300),
        read_plan_reply(&mut owner, Duration::from_secs(30)),
    )
    .await;
    assert!(
        blocked.is_err(),
        "an update_plan targeting a plan whose lock the test already holds must not resolve \
         while the test keeps the lock"
    );

    let mut closer = approver_connection(&h).await;
    send(
        &mut closer,
        &ClientMessage::CloseWindow {
            window_id: owner_window.clone(),
            force: None,
        },
    )
    .await;
    match read_response(&mut closer, Duration::from_secs(10)).await {
        DaemonMessage::WindowClosed { .. } => {}
        other => panic!("expected window_closed, got {other:?}"),
    }

    drop(held);

    let reply = read_plan_reply(&mut owner, Duration::from_secs(10)).await;
    expect_error(reply, "not_plan_owner");
    let read_by_approver = expect_plan_detail(get_plan(&mut closer, &plan.plan_id).await);
    assert_eq!(
        read_by_approver.tasks.len(),
        1,
        "the update that lost the authorization re-check must not have applied"
    );
}

// The lock table never grows for plans that do not exist, and shrinks
// when a plan is deleted.

#[tokio::test]
async fn the_lock_table_never_grows_for_nonexistent_plans_and_shrinks_on_delete() {
    let dir = unique_temp_dir("ipc-plan-lock-table-bound");
    let (h, locks) = start_ledger("ipc-plan-lock-table-bound", ledger_config(&dir, &[])).await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let mut approver = approver_connection(&h).await;

    assert_eq!(locks.len(), 0);
    for id in ["no-such-plan-1", "no-such-plan-2", "no-such-plan-3"] {
        expect_error(get_plan(&mut owner, id).await, "plan_not_found");
        expect_error(
            update_plan(&mut owner, id, tasks(&["t1"])).await,
            "plan_not_found",
        );
        expect_error(delete_plan(&mut owner, id).await, "plan_not_found");
    }
    assert_eq!(
        locks.len(),
        0,
        "a plan_id that never existed must never create a lock-table entry"
    );

    let plan = create_ok(&mut owner, "plan", tasks(&["t1"])).await;
    assert_eq!(
        locks.len(),
        0,
        "creating a plan alone must not create a lock-table entry (create is issued without a \
         guard)"
    );

    expect_plan_detail(update_plan(&mut owner, &plan.plan_id, tasks(&["t1", "t2"])).await);
    assert_eq!(
        locks.len(),
        1,
        "a successful update must leave the plan's own lock-table entry behind"
    );

    expect_plan_deleted(delete_plan(&mut approver, &plan.plan_id).await);
    assert_eq!(
        locks.len(),
        0,
        "deleting a plan must remove its lock-table entry"
    );

    // A concurrent update (already queued behind the test's held guard) and
    // an approver's delete: whichever wins the race, the plan ends up
    // deleted and its entry removed either way.
    let raced = create_ok(&mut owner, "plan", tasks(&["t1"])).await;
    let held = locks.lock(&raced.plan_id).await;
    assert_eq!(locks.len(), 1);

    send(
        &mut owner,
        &ClientMessage::UpdatePlan {
            plan_id: raced.plan_id.clone(),
            title: None,
            tasks: tasks(&["t1", "t2"]),
        },
    )
    .await;
    send(
        &mut approver,
        &ClientMessage::DeletePlan {
            plan_id: raced.plan_id.clone(),
        },
    )
    .await;

    drop(held);

    let update_reply = read_plan_reply(&mut owner, Duration::from_secs(10)).await;
    let delete_reply = read_plan_reply(&mut approver, Duration::from_secs(10)).await;

    match update_reply {
        DaemonMessage::PlanDetail { .. } => {}
        DaemonMessage::Error { code, .. } if code == "plan_not_found" => {}
        other => panic!("expected plan_detail or plan_not_found, got {other:?}"),
    }
    let deleted = expect_plan_deleted(delete_reply);
    assert_eq!(deleted, raced.plan_id);

    assert_eq!(
        locks.len(),
        0,
        "once the race resolves, the plan is deleted either way and its lock-table entry must \
         be gone"
    );
}
