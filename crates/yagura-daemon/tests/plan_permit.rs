#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Contracts for `yagura_daemon::plan_permit`: the plan ledger's proof-of-authorization
//! values (`CreatePermit`, `ChangePermit`, `ReadPermit`, `NominatePermit`) and its
//! per-plan lock table (`LockTable`).
//!
//! Every issuing function here reads the real `WindowRegistry` liveness predicate and a
//! real `PlanStore`; nothing in this file hands an issuing function a caller-supplied
//! fact to build a permit from. Children are `echo_child` or `sleepy_command`, never a
//! real vendor CLI.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use tokio::time::sleep;

use yagura_daemon::DaemonConfig;
use yagura_daemon::control::RequestOrigin;
use yagura_daemon::plan::{PlanSpec, TaskSpec, ValidatedPlan, validate_plan};
use yagura_daemon::plan_permit::{self, ChangeKind, LockTable, PermitError};
use yagura_daemon::plan_store::PlanStore;
use yagura_daemon::window::{OpenWindowRequest, WindowRegistry};
use yagura_protocol::WindowStatus;
use yagura_test_support::process::{echo_child_path, sleepy_command};

fn db_path(tag: &str) -> PathBuf {
    common::unique_temp_dir(tag).join("plan.db")
}

fn echo_request() -> OpenWindowRequest {
    let child = echo_child_path();
    OpenWindowRequest {
        command: vec![
            child
                .to_str()
                .expect("echo_child path must be valid UTF-8")
                .to_string(),
        ],
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
    }
}

/// A window whose direct child stays alive for roughly two seconds and then exits on
/// its own: enough time for an "alive" observation to land before the later "exited"
/// observation, without an explicit per-window kill (`WindowRegistry` exposes none).
fn short_lived_request() -> OpenWindowRequest {
    OpenWindowRequest {
        command: sleepy_command("3"),
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
    }
}

async fn open_window(registry: &WindowRegistry) -> String {
    registry
        .open(echo_request())
        .await
        .expect("open should succeed")
        .window_id
}

async fn open_short_lived_window(registry: &WindowRegistry) -> String {
    registry
        .open(short_lived_request())
        .await
        .expect("open should succeed")
        .window_id
}

/// Polls a registry-held entry until it reports `Exited`; panics on timeout. Mirrors
/// `tests/window_liveness.rs`'s helper of the same purpose (each integration test
/// binary is its own crate, so the helper cannot be shared directly).
async fn wait_for_exited(registry: &WindowRegistry, window_id: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let entry = registry
            .get(window_id)
            .expect("window must still be registered while waiting for it to exit");
        if entry.current_status() == WindowStatus::Exited {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            // What each layer last saw tells a missed child exit from a missed
            // status transition when this fires.
            panic!(
                "timed out waiting for the window to become Exited: raw exit status {:?}, \
                 current status {:?}, detector attached {}, detector task alive {}",
                entry.session().exit_status(),
                entry.current_status(),
                entry.has_detector(),
                entry.detector_task_alive(),
            );
        }
        sleep(Duration::from_millis(50)).await;
    }
}

/// A single-task plan with one write target, valid against `validate_plan` with every
/// profile id accepted.
fn one_task_plan(task_id: &str) -> ValidatedPlan {
    let spec = PlanSpec {
        title: "permit fixture plan".to_string(),
        tasks: vec![TaskSpec {
            task_id: task_id.to_string(),
            title: "do the thing".to_string(),
            role: None,
            profile: Some("codex-cli".to_string()),
            model: None,
            effort: None,
            depends_on: Vec::new(),
            writes: vec!["a.txt".to_string()],
            instruction: None,
        }],
    };
    validate_plan(&spec, |_| true).expect("a single-task plan with no cycles must validate")
}

/// Opens a fresh store and registry, creates one plan owned by a freshly opened alive
/// window, and returns `(store, registry, owner_window_id, plan_id)`. The convenience
/// every authorization case in this file that needs an existing plan to authorize
/// against starts from.
async fn store_registry_and_owned_plan(tag: &str) -> (PlanStore, WindowRegistry, String, String) {
    let mut store = PlanStore::open(&db_path(tag)).expect("PlanStore::open must succeed");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let owner = open_window(&registry).await;
    let create_permit = plan_permit::issue_create(&RequestOrigin::Window(owner.clone()), &registry)
        .expect("a freshly opened window must be able to create a plan");
    let plan_id = store
        .create_plan(&create_permit, &one_task_plan("t1"), None)
        .expect("create_plan must succeed for a valid single-task plan");
    (store, registry, owner, plan_id)
}

// create_plan's permit requires the calling window to be alive, and only a window may
// hold one at all.

#[tokio::test]
async fn issuing_a_create_permit_requires_the_calling_window_to_be_alive() {
    let registry = WindowRegistry::new(DaemonConfig::default());

    let alive = open_window(&registry).await;
    assert!(
        plan_permit::issue_create(&RequestOrigin::Window(alive), &registry).is_ok(),
        "a window with a running child must be able to create a plan"
    );

    let dying = open_short_lived_window(&registry).await;
    wait_for_exited(&registry, &dying).await;
    assert!(
        matches!(
            plan_permit::issue_create(&RequestOrigin::Window(dying), &registry),
            Err(PermitError::InvalidState)
        ),
        "a window whose child has already exited must not be able to create a plan"
    );

    assert!(
        matches!(
            plan_permit::issue_create(
                &RequestOrigin::Window("w999999-never".to_string()),
                &registry
            ),
            Err(PermitError::InvalidState)
        ),
        "an id this registry never opened must be refused the same way a dead window is, not \
         distinguished from it"
    );

    assert!(
        matches!(
            plan_permit::issue_create(&RequestOrigin::External, &registry),
            Err(PermitError::AuthFailed)
        ),
        "an unidentified connection must never create a plan"
    );
    assert!(
        matches!(
            plan_permit::issue_create(&RequestOrigin::Approver, &registry),
            Err(PermitError::AuthFailed)
        ),
        "an approver connection cannot create a plan on a window's behalf"
    );
}

// The change-authorization table, by origin/relation and by ChangeKind.

#[tokio::test]
async fn change_authorization_follows_the_origin_and_relation_table_for_structural_kinds() {
    let (store, registry, owner, plan_id) =
        store_registry_and_owned_plan("plan-permit-change-structural").await;
    let table = LockTable::new();

    for kind in [ChangeKind::UpdatePlan, ChangeKind::OpenTaskWindow] {
        let guard = table.lock(&plan_id).await;
        assert!(
            plan_permit::issue_change(
                &RequestOrigin::Window(owner.clone()),
                kind,
                guard,
                &store,
                &registry
            )
            .is_ok(),
            "the plan's alive owner must be authorized for {kind:?}"
        );

        let guard = table.lock(&plan_id).await;
        let unrelated = open_window(&registry).await;
        let result = plan_permit::issue_change(
            &RequestOrigin::Window(unrelated),
            kind,
            guard,
            &store,
            &registry,
        );
        assert!(
            matches!(result, Err(PermitError::PlanNotFound)),
            "a window with no relation to the plan must be refused as though it does not exist \
             for {kind:?}, got {result:?}"
        );

        let guard = table.lock(&plan_id).await;
        let approver =
            plan_permit::issue_change(&RequestOrigin::Approver, kind, guard, &store, &registry);
        assert!(
            matches!(approver, Err(PermitError::NotPlanOwner)),
            "an approver must not be able to rewrite a plan's structure or bind a task window \
             for {kind:?}, got {approver:?}"
        );

        let guard = table.lock(&plan_id).await;
        let external =
            plan_permit::issue_change(&RequestOrigin::External, kind, guard, &store, &registry);
        assert!(
            matches!(external, Err(PermitError::AuthFailed)),
            "an unidentified connection must never be authorized for {kind:?}"
        );
    }
}

#[tokio::test]
async fn delete_plan_is_authorized_for_the_alive_owner_and_for_the_approver() {
    let (store, registry, owner, plan_id) =
        store_registry_and_owned_plan("plan-permit-change-delete").await;
    let table = LockTable::new();

    let guard = table.lock(&plan_id).await;
    assert!(
        plan_permit::issue_change(
            &RequestOrigin::Approver,
            ChangeKind::DeletePlan,
            guard,
            &store,
            &registry
        )
        .is_ok(),
        "unlike the structural kinds, an approver must be authorized to delete a plan"
    );

    let guard = table.lock(&plan_id).await;
    assert!(
        plan_permit::issue_change(
            &RequestOrigin::Window(owner),
            ChangeKind::DeletePlan,
            guard,
            &store,
            &registry
        )
        .is_ok(),
        "the plan's alive owner must also be authorized to delete it"
    );
}

// The alive -> dead sequence below re-issues authorization for the same window and gets
// a different answer, so the fact must be re-read on every call rather than cached from
// the first issuance.

#[tokio::test]
async fn change_authorization_treats_a_dead_owner_the_same_as_a_bound_non_owner() {
    let (mut store, registry, owner, plan_id) =
        store_registry_and_owned_plan("plan-permit-change-dead-owner").await;
    let table = LockTable::new();

    // Bind a second window to the plan's task, authorized by the owner while alive: the
    // fixture for "range-in non-owner" every case below needs.
    let bound = open_window(&registry).await;
    let bind_guard = table.lock(&plan_id).await;
    let bind_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner.clone()),
        ChangeKind::OpenTaskWindow,
        bind_guard,
        &store,
        &registry,
    )
    .expect("the alive owner must be able to bind a task window");
    store
        .set_task_window(&bind_permit, "t1", &bound)
        .expect("set_task_window must succeed for the plan's own task");
    drop(bind_permit);

    let range_guard = table.lock(&plan_id).await;
    let range_result = plan_permit::issue_change(
        &RequestOrigin::Window(bound),
        ChangeKind::UpdatePlan,
        range_guard,
        &store,
        &registry,
    );
    assert!(
        matches!(range_result, Err(PermitError::NotPlanOwner)),
        "a window bound to one of the plan's tasks, but not its owner, must be refused \
         not_plan_owner rather than plan_not_found, got {range_result:?}"
    );

    // Now the owner itself dies (its child exits on its own), and a fresh authorization
    // attempt for the same window must flip from allowed to not_plan_owner.
    //
    // A dedicated dying owner is used here rather than reusing the already-opened one,
    // so the fixture's window_id used above for the range case stays interpretable.
    let (mut dying_store, dying_registry, _dying_owner, dying_plan_id) =
        store_registry_and_owned_plan("plan-permit-change-dying-owner").await;
    let dying_table = LockTable::new();

    // Replace the alive owner in this second fixture with a short-lived one by nominating
    // it: exercises the same "owner window id" field write path the real flow uses, so the
    // window that eventually dies really is the plan's owner_window value.
    let short_lived = open_short_lived_window(&dying_registry).await;
    let nominate_guard = dying_table.lock(&dying_plan_id).await;
    let nominate_permit = plan_permit::issue_nominate(
        &RequestOrigin::Approver,
        nominate_guard,
        &short_lived,
        &dying_registry,
    )
    .expect("an approver must be able to nominate an alive window as owner");
    dying_store
        .set_owner(nominate_permit)
        .expect("set_owner must persist the nominated owner");

    let alive_guard = dying_table.lock(&dying_plan_id).await;
    assert!(
        plan_permit::issue_change(
            &RequestOrigin::Window(short_lived.clone()),
            ChangeKind::UpdatePlan,
            alive_guard,
            &dying_store,
            &dying_registry
        )
        .is_ok(),
        "the newly nominated owner must be authorized while its child is still running"
    );

    wait_for_exited(&dying_registry, &short_lived).await;

    let dead_guard = dying_table.lock(&dying_plan_id).await;
    let dead_result = plan_permit::issue_change(
        &RequestOrigin::Window(short_lived),
        ChangeKind::UpdatePlan,
        dead_guard,
        &dying_store,
        &dying_registry,
    );
    assert!(
        matches!(dead_result, Err(PermitError::NotPlanOwner)),
        "the same window, once its child has exited, must be refused not_plan_owner, got \
         {dead_result:?}"
    );

    // The out-of-range and not-plan-owner refusals must be textually indistinguishable
    // from one another where the table calls for `plan_not_found`, and stable across
    // both causes where it calls for `not_plan_owner`.
    assert_eq!(
        range_result.unwrap_err().to_string(),
        dead_result.unwrap_err().to_string(),
        "a bound non-owner and a dead owner must produce byte-identical not_plan_owner \
         messages, or the two refusals would leak which case applies"
    );

    let out_of_range_guard = table.lock(&plan_id).await;
    let out_of_range_result = plan_permit::issue_change(
        &RequestOrigin::Window(open_window(&registry).await),
        ChangeKind::UpdatePlan,
        out_of_range_guard,
        &store,
        &registry,
    );
    let nonexistent_guard = table.lock("plan-does-not-exist-at-all").await;
    let nonexistent_result = plan_permit::issue_change(
        &RequestOrigin::Window(open_window(&registry).await),
        ChangeKind::UpdatePlan,
        nonexistent_guard,
        &store,
        &registry,
    );
    assert_eq!(
        out_of_range_result.unwrap_err().to_string(),
        nonexistent_result.unwrap_err().to_string(),
        "a window out of a real plan's range and a window naming a plan_id that does not \
         exist at all must produce byte-identical plan_not_found messages (the IPC layer's \
         own indistinguishability guarantee rests on this)"
    );
}

// The read range.

#[tokio::test]
async fn read_authorization_admits_owner_bound_and_approver_but_not_out_of_range_or_external() {
    let (mut store, registry, owner, plan_id) =
        store_registry_and_owned_plan("plan-permit-read-range").await;
    let table = LockTable::new();

    assert!(
        plan_permit::issue_read(
            &RequestOrigin::Window(owner.clone()),
            &plan_id,
            &store,
            &registry
        )
        .is_ok(),
        "the alive owner must be able to read its own plan"
    );

    let bound = open_window(&registry).await;
    let bind_guard = table.lock(&plan_id).await;
    let bind_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner.clone()),
        ChangeKind::OpenTaskWindow,
        bind_guard,
        &store,
        &registry,
    )
    .expect("the owner must be able to bind a task window");
    store
        .set_task_window(&bind_permit, "t1", &bound)
        .expect("set_task_window must succeed");
    assert!(
        plan_permit::issue_read(&RequestOrigin::Window(bound), &plan_id, &store, &registry).is_ok(),
        "a window bound to one of the plan's tasks must be able to read it, even though it is \
         not the owner"
    );

    let out_of_range = open_window(&registry).await;
    let out_of_range_result = plan_permit::issue_read(
        &RequestOrigin::Window(out_of_range),
        &plan_id,
        &store,
        &registry,
    );
    assert!(
        matches!(out_of_range_result, Err(PermitError::PlanNotFound)),
        "a window with no relation to the plan must be refused as though it does not exist, \
         got {out_of_range_result:?}"
    );

    assert!(
        plan_permit::issue_read(&RequestOrigin::Approver, &plan_id, &store, &registry).is_ok(),
        "an approver must be able to read any plan"
    );
    assert!(
        matches!(
            plan_permit::issue_read(&RequestOrigin::External, &plan_id, &store, &registry),
            Err(PermitError::AuthFailed)
        ),
        "an unidentified connection must never read a plan"
    );

    // Reading does not care whether the owner is alive: a dead owner is still read as
    // the owner. This is the read-side counterpart of the change-authorization case
    // above that flips a live owner to a dead one and re-checks.
    let (dying_store, dying_registry, dying_owner, dying_plan_id) =
        store_registry_and_owned_plan("plan-permit-read-range-dead-owner").await;
    assert!(
        plan_permit::issue_read(
            &RequestOrigin::Window(dying_owner.clone()),
            &dying_plan_id,
            &dying_store,
            &dying_registry
        )
        .is_ok(),
        "the owner must be able to read its own plan while alive"
    );
    let dying_window_id = open_short_lived_window(&dying_registry).await;
    let dying_table = LockTable::new();
    let nominate_guard = dying_table.lock(&dying_plan_id).await;
    let nominate_permit = plan_permit::issue_nominate(
        &RequestOrigin::Approver,
        nominate_guard,
        &dying_window_id,
        &dying_registry,
    )
    .expect("an approver must be able to nominate an alive window as owner");
    let mut dying_store = dying_store;
    dying_store
        .set_owner(nominate_permit)
        .expect("set_owner must persist the nominated owner");
    wait_for_exited(&dying_registry, &dying_window_id).await;
    assert!(
        plan_permit::issue_read(
            &RequestOrigin::Window(dying_window_id),
            &dying_plan_id,
            &dying_store,
            &dying_registry
        )
        .is_ok(),
        "once the owner's child has exited, the owner must still be able to read its own \
         plan -- reading never checks liveness"
    );
}

// Approver-only operations never reveal whether the plan exists.

#[tokio::test]
async fn approver_only_operations_reject_every_window_origin_even_for_a_nonexistent_plan() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let window = open_window(&registry).await;
    let store = PlanStore::open(&db_path("plan-permit-approver-only")).expect("PlanStore::open");
    let table = LockTable::new();

    for kind in [
        ChangeKind::SetTaskAssignment,
        ChangeKind::ClearTaskAssignment,
    ] {
        let guard = table.lock("plan-does-not-exist").await;
        let result = plan_permit::issue_change(
            &RequestOrigin::Window(window.clone()),
            kind,
            guard,
            &store,
            &registry,
        );
        assert!(
            matches!(result, Err(PermitError::AuthFailed)),
            "{kind:?} from a window must be auth_failed even for a plan_id that does not exist, \
             so existence is never revealed by which error comes back, got {result:?}"
        );
    }

    let nominate_guard = table.lock("plan-does-not-exist-2").await;
    let nominate_result = plan_permit::issue_nominate(
        &RequestOrigin::Window(window),
        nominate_guard,
        "w-some-candidate",
        &registry,
    );
    assert!(
        matches!(nominate_result, Err(PermitError::AuthFailed)),
        "nominating an owner from a window must be auth_failed the same way, got \
         {nominate_result:?}"
    );
}

// A plan-scoped lock admits one guard at a time; different plans are independent;
// deleting a plan removes its lock-table entry.

#[tokio::test]
async fn a_plan_lock_admits_one_guard_at_a_time_and_different_plans_are_independent() {
    let table = LockTable::new();

    let first = table.lock("plan-a").await;
    let blocked = tokio::time::timeout(Duration::from_millis(200), table.lock("plan-a")).await;
    assert!(
        blocked.is_err(),
        "a second lock attempt on the same plan must not resolve while the first is held"
    );

    let independent = tokio::time::timeout(Duration::from_millis(200), table.lock("plan-b")).await;
    assert!(
        independent.is_ok(),
        "a lock on a different plan must not wait on plan-a's guard"
    );

    drop(first);
    let queued = tokio::time::timeout(Duration::from_millis(1000), table.lock("plan-a")).await;
    assert!(
        queued.is_ok(),
        "releasing the first guard must let a queued lock on the same plan proceed"
    );
}

#[tokio::test]
async fn removing_a_plan_shrinks_the_lock_table() {
    let table = LockTable::new();

    {
        let _guard = table.lock("plan-x").await;
        assert_eq!(
            table.len(),
            1,
            "acquiring a lock must create the plan's entry"
        );
    }
    assert_eq!(
        table.len(),
        1,
        "releasing a guard must not by itself remove the plan's entry -- entries are created \
         for existing plans and removed on deletion, not on every unlock"
    );

    table.remove("plan-x");
    assert_eq!(
        table.len(),
        0,
        "removing a deleted plan's entry must shrink the lock table"
    );
}

// An issued ChangePermit can move into a `spawn_blocking` closure and still license a
// write once inside it (the door `StoreGate::with_store` -- src/ipc/chat.rs -- requires
// `F: Send + 'static`, so this is the shape production code must compile).

#[tokio::test]
async fn a_change_permit_moved_into_spawn_blocking_still_licenses_a_store_write() {
    let (store, registry, owner, plan_id) =
        store_registry_and_owned_plan("plan-permit-spawn-blocking").await;
    let table = LockTable::new();

    let guard = table.lock(&plan_id).await;
    let permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner),
        ChangeKind::UpdatePlan,
        guard,
        &store,
        &registry,
    )
    .expect("the alive owner must be authorized to update the plan");

    let store = std::sync::Arc::new(std::sync::Mutex::new(store));
    let store_in_closure = store.clone();
    let replacement = one_task_plan("t1-replaced");
    let outcome = tokio::task::spawn_blocking(move || {
        let mut guard = store_in_closure
            .lock()
            .expect("the store mutex must not be poisoned");
        guard.replace_tasks(&permit, &replacement)
    })
    .await
    .expect("the blocking task must not panic or be cancelled");

    assert!(
        outcome.is_ok(),
        "a ChangePermit moved into a spawn_blocking closure must still license the write it \
         was issued for, got {outcome:?}"
    );

    let store = store.lock().expect("the store mutex must not be poisoned");
    let detail = store
        .get_plan(&plan_id)
        .expect("get_plan must not error")
        .expect("the plan must still exist");
    assert_eq!(
        detail
            .tasks
            .iter()
            .map(|t| t.task_id.as_str())
            .collect::<Vec<_>>(),
        vec!["t1-replaced"],
        "the write issued through the moved permit must actually have landed"
    );
}
