#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Seeds and reads the database with raw SQL, which only tests may do.
#![allow(clippy::disallowed_methods)]

//! Contracts for how a plan's review state enters `plan_permit::issue_change`: a
//! pending plan's owner cannot open a task window yet, the refusal is decided only
//! after the caller's relation to the plan (so it never reveals the review state to a
//! window that could not act anyway), and approving or marking a plan seen is an
//! approver-only licence.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::params;
use tokio::time::sleep;

use yagura_daemon::DaemonConfig;
use yagura_daemon::control::RequestOrigin;
use yagura_daemon::plan::{PlanSpec, TaskSpec, ValidatedPlan, validate_plan};
use yagura_daemon::plan_permit::{self, ChangeKind, LockTable, PermitError};
use yagura_daemon::plan_store::{PlanReview, PlanStore};
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
        on_name_taken: Default::default(),
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
        .open(OpenWindowRequest {
            command: sleepy_command("3"),
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        })
        .await
        .expect("open should succeed")
        .window_id
}

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
            panic!("timed out waiting for the window to become Exited");
        }
        sleep(Duration::from_millis(50)).await;
    }
}

fn one_task_plan() -> ValidatedPlan {
    let spec = PlanSpec {
        title: "review permit fixture".to_string(),
        tasks: vec![TaskSpec {
            task_id: "t1".to_string(),
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
    validate_plan(&spec, |_| true).expect("a single-task plan must validate")
}

fn create(
    store: &mut PlanStore,
    registry: &WindowRegistry,
    owner: &str,
    review: Option<PlanReview>,
) -> String {
    let permit = plan_permit::issue_create(&RequestOrigin::Window(owner.to_string()), registry)
        .expect("an alive window must be able to create a plan");
    store
        .create_plan(&permit, &one_task_plan(), review)
        .expect("create_plan must succeed")
}

async fn approve(
    store: &mut PlanStore,
    registry: &WindowRegistry,
    table: &LockTable,
    plan_id: &str,
) {
    let guard = table.lock(plan_id).await;
    let permit = plan_permit::issue_change(
        &RequestOrigin::Approver,
        ChangeKind::ApprovePlan,
        guard,
        store,
        registry,
    )
    .expect("the approver must be able to approve a plan");
    store
        .approve_plan(&permit, 0)
        .expect("approve_plan must not fail on storage");
}

async fn change(
    table: &LockTable,
    store: &PlanStore,
    registry: &WindowRegistry,
    origin: RequestOrigin,
    kind: ChangeKind,
    plan_id: &str,
) -> Result<(), PermitError> {
    let guard = table.lock(plan_id).await;
    plan_permit::issue_change(&origin, kind, guard, store, registry).map(drop)
}

/// Binds `window_id` to the plan's task directly in the database, so a pending plan
/// can have a bound window without first passing the check this file is about.
fn bind_directly(path: &Path, plan_id: &str, window_id: &str) {
    let conn = rusqlite::Connection::open(path).expect("open the plan database directly");
    let updated = conn
        .execute(
            "UPDATE plan_task SET window_id = ?1 WHERE plan_id = ?2 AND task_id = 't1'",
            params![window_id, plan_id],
        )
        .expect("binding the task directly must succeed");
    assert_eq!(updated, 1);
}

#[tokio::test]
async fn a_pending_plans_alive_owner_cannot_open_a_task_window() {
    let mut store = PlanStore::open(&db_path("plan-review-permit-pending")).unwrap();
    let registry = WindowRegistry::new(DaemonConfig::default());
    let table = LockTable::new();
    let owner = open_window(&registry).await;

    let pending = create(&mut store, &registry, &owner, Some(PlanReview::Pending));
    let result = change(
        &table,
        &store,
        &registry,
        RequestOrigin::Window(owner.clone()),
        ChangeKind::OpenTaskWindow,
        &pending,
    )
    .await;
    assert_eq!(
        result,
        Err(PermitError::PlanNotApproved),
        "a plan the operator has not approved must not open its task windows"
    );

    for kind in [ChangeKind::UpdatePlan, ChangeKind::DeletePlan] {
        let result = change(
            &table,
            &store,
            &registry,
            RequestOrigin::Window(owner.clone()),
            kind,
            &pending,
        )
        .await;
        assert_eq!(
            result,
            Ok(()),
            "the owner must still be able to {kind:?} while its plan waits for the operator"
        );
    }
}

#[tokio::test]
async fn an_approved_or_unreviewed_plans_alive_owner_can_open_a_task_window() {
    let mut store = PlanStore::open(&db_path("plan-review-permit-open")).unwrap();
    let registry = WindowRegistry::new(DaemonConfig::default());
    let table = LockTable::new();
    let owner = open_window(&registry).await;

    let approved = create(&mut store, &registry, &owner, Some(PlanReview::Pending));
    approve(&mut store, &registry, &table, &approved).await;
    let unreviewed = create(&mut store, &registry, &owner, None);

    for plan_id in [&approved, &unreviewed] {
        let result = change(
            &table,
            &store,
            &registry,
            RequestOrigin::Window(owner.clone()),
            ChangeKind::OpenTaskWindow,
            plan_id,
        )
        .await;
        assert_eq!(
            result,
            Ok(()),
            "the alive owner of an approved or unreviewed plan must be able to open a task \
             window"
        );
    }
}

#[tokio::test]
async fn a_window_that_could_not_open_a_task_anyway_gets_the_same_refusal_whether_or_not_the_plan_is_pending()
 {
    let path = db_path("plan-review-permit-relation");
    let mut store = PlanStore::open(&path).unwrap();
    let registry = WindowRegistry::new(DaemonConfig::default());
    let table = LockTable::new();

    let owner = open_window(&registry).await;
    let pending = create(&mut store, &registry, &owner, Some(PlanReview::Pending));
    let unreviewed = create(&mut store, &registry, &owner, None);
    let bound = open_window(&registry).await;
    bind_directly(&path, &pending, &bound);
    bind_directly(&path, &unreviewed, &bound);
    let unrelated = open_window(&registry).await;

    let dying = open_short_lived_window(&registry).await;
    let dying_pending = create(&mut store, &registry, &dying, Some(PlanReview::Pending));
    let dying_unreviewed = create(&mut store, &registry, &dying, None);
    wait_for_exited(&registry, &dying).await;

    let cases = [
        (
            "a bound non-owner",
            bound,
            &pending,
            &unreviewed,
            PermitError::NotPlanOwner,
        ),
        (
            "an unrelated window",
            unrelated,
            &pending,
            &unreviewed,
            PermitError::PlanNotFound,
        ),
        (
            "a dead owner",
            dying,
            &dying_pending,
            &dying_unreviewed,
            PermitError::NotPlanOwner,
        ),
    ];
    for (who, window, pending_id, unreviewed_id, expected) in cases {
        let on_pending = change(
            &table,
            &store,
            &registry,
            RequestOrigin::Window(window.clone()),
            ChangeKind::OpenTaskWindow,
            pending_id,
        )
        .await;
        let on_unreviewed = change(
            &table,
            &store,
            &registry,
            RequestOrigin::Window(window),
            ChangeKind::OpenTaskWindow,
            unreviewed_id,
        )
        .await;
        assert_eq!(
            on_pending,
            Err(expected),
            "{who} must be refused by its relation before the review state is looked at"
        );
        assert_eq!(
            on_pending.unwrap_err().to_string(),
            on_unreviewed.unwrap_err().to_string(),
            "{who} must not be able to tell a pending plan from an unreviewed one"
        );
    }

    let missing = change(
        &table,
        &store,
        &registry,
        RequestOrigin::Window(owner),
        ChangeKind::OpenTaskWindow,
        "p-does-not-exist",
    )
    .await;
    assert_eq!(missing, Err(PermitError::PlanNotFound));
}

#[tokio::test]
async fn approving_and_marking_seen_are_licensed_only_for_the_approver() {
    let mut store = PlanStore::open(&db_path("plan-review-permit-approver")).unwrap();
    let registry = WindowRegistry::new(DaemonConfig::default());
    let table = LockTable::new();
    let owner = open_window(&registry).await;
    let pending = create(&mut store, &registry, &owner, Some(PlanReview::Pending));

    for kind in [ChangeKind::ApprovePlan, ChangeKind::MarkPlanSeen] {
        for plan_id in [pending.as_str(), "p-does-not-exist"] {
            let from_owner = change(
                &table,
                &store,
                &registry,
                RequestOrigin::Window(owner.clone()),
                kind,
                plan_id,
            )
            .await;
            assert_eq!(
                from_owner,
                Err(PermitError::AuthFailed),
                "a window, even the plan's alive owner, must never hold {kind:?}"
            );

            let from_external = change(
                &table,
                &store,
                &registry,
                RequestOrigin::External,
                kind,
                plan_id,
            )
            .await;
            assert_eq!(from_external, Err(PermitError::AuthFailed));

            let from_approver = change(
                &table,
                &store,
                &registry,
                RequestOrigin::Approver,
                kind,
                plan_id,
            )
            .await;
            assert_eq!(
                from_approver,
                Ok(()),
                "the approver must be licensed for {kind:?} without the plan being read first"
            );
        }
    }
}
