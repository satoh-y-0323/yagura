#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Seeds and reads the database with raw SQL, which only tests may do.
#![allow(clippy::disallowed_methods)]

//! Contracts for `yagura_daemon::plan_store::PlanStore`, the plan ledger's SQLite
//! persistence port (migration 007). Every write method here is reached through a
//! `plan_permit` value obtained from the real issuing functions, never a
//! hand-built one; `tests/plan_permit.rs` is the authorization contract for those
//! issuing functions themselves, so this file assumes they work and focuses on what
//! the store does with a permit once it holds one.
//!
//! Schema-shape contracts (the exact v7 DDL, and v6 + migration 007 landing on the same
//! schema as a fresh v7 database) live in `tests/store_migrations.rs`, next to the
//! existing per-version schema fixtures they extend.

mod common;

use std::path::PathBuf;

use rusqlite::params;

use yagura_daemon::DaemonConfig;
use yagura_daemon::control::RequestOrigin;
use yagura_daemon::plan::{Assignment, PlanSpec, TaskSpec, ValidatedPlan, validate_plan};
use yagura_daemon::plan_permit::{self, ChangeKind, LockTable};
use yagura_daemon::plan_store::{PlanStore, PlanStoreError, ResourceLimitKind};
use yagura_daemon::store;
use yagura_daemon::window::{OpenWindowRequest, WindowRegistry};
use yagura_test_support::process::echo_child_path;

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

/// A single-task plan (`task_id = "t1"`), valid against `validate_plan` with every
/// profile id accepted.
fn one_task_plan(task_id: &str) -> ValidatedPlan {
    let spec = PlanSpec {
        title: "single task fixture".to_string(),
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

/// Two independent tasks (`t1`, `t2`), no dependency between them, distinct write
/// targets.
fn two_task_plan() -> ValidatedPlan {
    let spec = PlanSpec {
        title: "two task fixture".to_string(),
        tasks: vec![
            TaskSpec {
                task_id: "t1".to_string(),
                title: "first".to_string(),
                role: None,
                profile: Some("codex-cli".to_string()),
                model: None,
                effort: None,
                depends_on: Vec::new(),
                writes: vec!["a.txt".to_string()],
                instruction: None,
            },
            TaskSpec {
                task_id: "t2".to_string(),
                title: "second".to_string(),
                role: None,
                profile: Some("codex-cli".to_string()),
                model: None,
                effort: None,
                depends_on: Vec::new(),
                writes: vec!["b.txt".to_string()],
                instruction: None,
            },
        ],
    };
    validate_plan(&spec, |_| true).expect("a two-task plan with no cycles must validate")
}

// create returns a unique id, cwd/owner come from the creating window, get_plan
// round-trips registration order / dependencies / write-target case / role / parent
// assignment, list_plans returns summaries, and delete removes every row the plan owns.

#[tokio::test]
async fn create_get_list_and_delete_round_trip_a_plan() {
    let path = db_path("plan-store-crud");
    let mut store = PlanStore::open(&path).expect("PlanStore::open must succeed");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let owner = open_window(&registry).await;
    let owner_cwd = registry
        .get(&owner)
        .expect("the owner window must be registered")
        .cwd()
        .to_string();

    let spec = PlanSpec {
        title: "release checklist".to_string(),
        tasks: vec![
            TaskSpec {
                task_id: "b".to_string(),
                title: "second".to_string(),
                role: Some("impl".to_string()),
                profile: Some("codex-cli".to_string()),
                model: None,
                effort: None,
                depends_on: vec!["a".to_string()],
                writes: vec!["src/B.rs".to_string()],
                instruction: None,
            },
            TaskSpec {
                task_id: "a".to_string(),
                title: "first".to_string(),
                role: None,
                profile: Some("claude-code".to_string()),
                model: Some("opus".to_string()),
                effort: Some("high".to_string()),
                depends_on: Vec::new(),
                writes: vec!["src/A.rs".to_string()],
                instruction: None,
            },
        ],
    };
    let validated = validate_plan(&spec, |_| true).expect("the fixture plan must validate");

    let permit = plan_permit::issue_create(&RequestOrigin::Window(owner.clone()), &registry)
        .expect("the alive owner must be able to create a plan");
    let plan_id = store
        .create_plan(&permit, &validated, None)
        .expect("create_plan must succeed for a valid plan");
    assert!(
        plan_id.starts_with("p-") && plan_id.len() > 2,
        "plan_id must be a nonempty identifier carrying the documented prefix, got {plan_id:?}"
    );

    let second_permit = plan_permit::issue_create(&RequestOrigin::Window(owner.clone()), &registry)
        .expect("the owner must be able to create a second plan");
    let second_plan_id = store
        .create_plan(&second_permit, &validated, None)
        .expect("create_plan must succeed a second time");
    assert_ne!(
        plan_id, second_plan_id,
        "two creations must never collide on the same plan_id"
    );

    let detail = store
        .get_plan(&plan_id)
        .expect("get_plan must not error")
        .expect("the created plan must be readable");
    assert_eq!(detail.owner_window.as_deref(), Some(owner.as_str()));
    assert_eq!(
        detail.cwd, owner_cwd,
        "a plan's cwd must be the creating window's cwd, taken from the create permit, not \
         supplied as a separate argument"
    );

    let ids: Vec<&str> = detail.tasks.iter().map(|t| t.task_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["b", "a"],
        "tasks must come back in registration order (b was declared first), not sorted by id"
    );

    let task_b = detail
        .tasks
        .iter()
        .find(|t| t.task_id == "b")
        .expect("task b must exist");
    assert_eq!(task_b.depends_on, vec!["a".to_string()]);
    assert_eq!(
        task_b.writes,
        vec!["src/B.rs".to_string()],
        "a write target's original case must survive the round trip"
    );
    assert_eq!(task_b.role.as_deref(), Some("impl"));
    assert_eq!(
        task_b.parent,
        Some(Assignment {
            profile: "codex-cli".to_string(),
            model: None,
            effort: None
        })
    );
    assert_eq!(task_b.user, None, "no user override has been set yet");
    assert_eq!(task_b.window_id, None, "no window has been bound yet");

    let task_a = detail
        .tasks
        .iter()
        .find(|t| t.task_id == "a")
        .expect("task a must exist");
    assert_eq!(
        task_a.parent,
        Some(Assignment {
            profile: "claude-code".to_string(),
            model: Some("opus".to_string()),
            effort: Some("high".to_string())
        })
    );

    let summaries = store.list_plans().expect("list_plans must not error");
    let summary = summaries
        .iter()
        .find(|s| s.plan_id == plan_id)
        .expect("list_plans must include the created plan");
    assert_eq!(summary.owner_window.as_deref(), Some(owner.as_str()));
    assert_eq!(summary.task_count, 2);

    let table = LockTable::new();
    let guard = table.lock(&plan_id).await;
    let delete_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner),
        ChangeKind::DeletePlan,
        guard,
        &store,
        &registry,
    )
    .expect("the alive owner must be authorized to delete the plan");
    store
        .delete_plan(delete_permit)
        .expect("delete_plan must succeed");

    assert!(
        store
            .get_plan(&plan_id)
            .expect("get_plan must not error")
            .is_none(),
        "a deleted plan must no longer be readable"
    );
    assert!(
        store
            .list_plans()
            .expect("list_plans must not error")
            .iter()
            .all(|s| s.plan_id != plan_id),
        "a deleted plan must not appear in list_plans"
    );

    let conn = store::open_readonly(&path).expect("open_readonly must succeed");
    let task_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM plan_task WHERE plan_id = ?1",
            params![plan_id],
            |row| row.get(0),
        )
        .expect("the plan_task count query must run");
    let dep_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM plan_task_dep WHERE plan_id = ?1",
            params![plan_id],
            |row| row.get(0),
        )
        .expect("the plan_task_dep count query must run");
    let write_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM plan_task_write WHERE plan_id = ?1",
            params![plan_id],
            |row| row.get(0),
        )
        .expect("the plan_task_write count query must run");
    assert_eq!(
        (task_count, dep_count, write_count),
        (0, 0, 0),
        "deleting a plan must remove every row it owns across plan_task, plan_task_dep and \
         plan_task_write, not just its own plan row"
    );
}

// Opening as a new boot clears owner_window and every task's window binding, but keeps
// the plan, its tasks and any user override.

#[tokio::test]
async fn opening_as_a_new_boot_clears_owner_and_window_bindings_but_keeps_everything_else() {
    let path = db_path("plan-store-new-boot");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let owner = open_window(&registry).await;
    let bound = open_window(&registry).await;

    let plan_id = {
        let mut store = PlanStore::open(&path).expect("PlanStore::open must succeed");
        let create_permit =
            plan_permit::issue_create(&RequestOrigin::Window(owner.clone()), &registry)
                .expect("the owner must be able to create a plan");
        let plan_id = store
            .create_plan(&create_permit, &one_task_plan("t1"), None)
            .expect("create_plan must succeed");

        let table = LockTable::new();
        let guard = table.lock(&plan_id).await;
        let bind_permit = plan_permit::issue_change(
            &RequestOrigin::Window(owner.clone()),
            ChangeKind::OpenTaskWindow,
            guard,
            &store,
            &registry,
        )
        .expect("the owner must be able to bind a task window");
        store
            .set_task_window(&bind_permit, "t1", &bound)
            .expect("set_task_window must succeed");
        plan_id
    };

    let store = PlanStore::open_as_new_boot(&path).expect("open_as_new_boot must succeed");
    let detail = store
        .get_plan(&plan_id)
        .expect("get_plan must not error")
        .expect("the plan must survive a new boot");
    assert_eq!(
        detail.owner_window, None,
        "a new boot must clear owner_window, since window ids do not survive a restart"
    );
    assert_eq!(
        detail.tasks[0].window_id, None,
        "a new boot must clear every task's window binding as well"
    );
    assert_eq!(detail.title, "single task fixture");
    assert_eq!(detail.tasks[0].task_id, "t1");
}

// Replacing a plan's tasks keeps the surviving task_ids' user overrides and window
// bindings, drops what belonged to a task_id that disappeared, and adopts the new
// registration order.

#[tokio::test]
async fn replacing_tasks_carries_over_surviving_state_and_drops_the_rest() {
    let path = db_path("plan-store-replace");
    let mut store = PlanStore::open(&path).expect("PlanStore::open must succeed");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let owner = open_window(&registry).await;
    let bound = open_window(&registry).await;

    let create_permit = plan_permit::issue_create(&RequestOrigin::Window(owner.clone()), &registry)
        .expect("the owner must be able to create a plan");
    let plan_id = store
        .create_plan(&create_permit, &two_task_plan(), None)
        .expect("create_plan must succeed");

    let table = LockTable::new();

    let override_guard = table.lock(&plan_id).await;
    let override_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner.clone()),
        ChangeKind::UpdatePlan,
        override_guard,
        &store,
        &registry,
    )
    .expect("the owner must be authorized to hold a change permit for this plan");
    store
        .set_user_override(
            &override_permit,
            "t1",
            Some(Assignment {
                profile: "claude-code".to_string(),
                model: None,
                effort: None,
            }),
        )
        .expect("set_user_override must succeed for t1");
    store
        .set_user_override(
            &override_permit,
            "t2",
            Some(Assignment {
                profile: "gemini-cli".to_string(),
                model: None,
                effort: None,
            }),
        )
        .expect("set_user_override must succeed for t2");
    drop(override_permit);

    let bind_guard = table.lock(&plan_id).await;
    let bind_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner.clone()),
        ChangeKind::OpenTaskWindow,
        bind_guard,
        &store,
        &registry,
    )
    .expect("the owner must be authorized to bind a task window");
    store
        .set_task_window(&bind_permit, "t1", &bound)
        .expect("set_task_window must succeed");
    drop(bind_permit);

    let replacement_spec = PlanSpec {
        title: "release checklist".to_string(),
        tasks: vec![
            TaskSpec {
                task_id: "t1".to_string(),
                title: "first, retitled".to_string(),
                role: None,
                profile: Some("codex-cli".to_string()),
                model: None,
                effort: None,
                depends_on: Vec::new(),
                writes: vec!["a.txt".to_string()],
                instruction: None,
            },
            TaskSpec {
                task_id: "t3".to_string(),
                title: "third, brand new".to_string(),
                role: None,
                profile: Some("codex-cli".to_string()),
                model: None,
                effort: None,
                depends_on: Vec::new(),
                writes: vec!["c.txt".to_string()],
                instruction: None,
            },
        ],
    };
    let replacement =
        validate_plan(&replacement_spec, |_| true).expect("the replacement plan must validate");

    let replace_guard = table.lock(&plan_id).await;
    let replace_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner),
        ChangeKind::UpdatePlan,
        replace_guard,
        &store,
        &registry,
    )
    .expect("the owner must be authorized to replace the plan's tasks");
    store
        .replace_tasks(&replace_permit, &replacement)
        .expect("replace_tasks must succeed");

    let detail = store
        .get_plan(&plan_id)
        .expect("get_plan must not error")
        .expect("the plan must still exist");
    let ids: Vec<&str> = detail.tasks.iter().map(|t| t.task_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["t1", "t3"],
        "the replaced task set must be exactly the new task_ids, in the new registration order; \
         t2 must be gone"
    );

    let t1 = detail
        .tasks
        .iter()
        .find(|t| t.task_id == "t1")
        .expect("t1 must survive the replace");
    assert_eq!(t1.title, "first, retitled");
    assert_eq!(
        t1.user,
        Some(Assignment {
            profile: "claude-code".to_string(),
            model: None,
            effort: None
        }),
        "a surviving task_id must keep its user override across a replace"
    );
    assert_eq!(
        t1.window_id,
        Some(bound),
        "a surviving task_id must keep its bound window across a replace"
    );

    let t3 = detail
        .tasks
        .iter()
        .find(|t| t.task_id == "t3")
        .expect("t3 must be present as a brand-new task");
    assert_eq!(
        t3.user, None,
        "a brand-new task_id must start with no user override"
    );
    assert_eq!(t3.window_id, None, "a brand-new task_id starts unbound");
}

// Setting and clearing a user override, setting the owner, and binding a task's window
// each round-trip, and a write made under one plan's permit never touches another plan.

#[tokio::test]
async fn overrides_owner_and_window_bindings_round_trip_and_stay_scoped_to_their_own_plan() {
    let path = db_path("plan-store-roundtrip");
    let mut store = PlanStore::open(&path).expect("PlanStore::open must succeed");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let owner_a = open_window(&registry).await;
    let owner_b = open_window(&registry).await;

    let permit_a = plan_permit::issue_create(&RequestOrigin::Window(owner_a.clone()), &registry)
        .expect("owner_a must be able to create a plan");
    let plan_a = store
        .create_plan(&permit_a, &one_task_plan("t1"), None)
        .expect("create_plan must succeed for plan A");
    let permit_b = plan_permit::issue_create(&RequestOrigin::Window(owner_b.clone()), &registry)
        .expect("owner_b must be able to create a plan");
    let plan_b = store
        .create_plan(&permit_b, &one_task_plan("t1"), None)
        .expect("create_plan must succeed for plan B");

    let table = LockTable::new();

    let guard_a = table.lock(&plan_a).await;
    let change_a = plan_permit::issue_change(
        &RequestOrigin::Window(owner_a.clone()),
        ChangeKind::UpdatePlan,
        guard_a,
        &store,
        &registry,
    )
    .expect("owner_a must hold a change permit for plan A");
    let override_value = Assignment {
        profile: "claude-code".to_string(),
        model: Some("opus".to_string()),
        effort: None,
    };
    store
        .set_user_override(&change_a, "t1", Some(override_value.clone()))
        .expect("set_user_override must succeed");
    drop(change_a);

    let detail_a = store
        .get_plan(&plan_a)
        .expect("get_plan must not error")
        .expect("plan A must exist");
    assert_eq!(detail_a.tasks[0].user, Some(override_value));

    let detail_b_before = store
        .get_plan(&plan_b)
        .expect("get_plan must not error")
        .expect("plan B must exist");
    assert_eq!(
        detail_b_before.tasks[0].user, None,
        "a write made under plan A's permit must never touch plan B"
    );

    let guard_a2 = table.lock(&plan_a).await;
    let change_a2 = plan_permit::issue_change(
        &RequestOrigin::Window(owner_a),
        ChangeKind::UpdatePlan,
        guard_a2,
        &store,
        &registry,
    )
    .expect("owner_a must hold a second change permit for plan A");
    store
        .set_user_override(&change_a2, "t1", None)
        .expect("clearing the override must succeed");
    drop(change_a2);
    let detail_a2 = store
        .get_plan(&plan_a)
        .expect("get_plan must not error")
        .expect("plan A must still exist");
    assert_eq!(
        detail_a2.tasks[0].user, None,
        "clearing an override must round-trip back to no override"
    );

    let guard_nominate = table.lock(&plan_a).await;
    let nominate_permit = plan_permit::issue_nominate(
        &RequestOrigin::Approver,
        guard_nominate,
        &owner_b,
        &registry,
    )
    .expect("an approver must be able to nominate an alive window as owner");
    store
        .set_owner(nominate_permit)
        .expect("set_owner must succeed");
    let detail_a3 = store
        .get_plan(&plan_a)
        .expect("get_plan must not error")
        .expect("plan A must still exist");
    assert_eq!(
        detail_a3.owner_window.as_deref(),
        Some(owner_b.as_str()),
        "set_owner must round-trip the newly nominated owner"
    );
}

// Closing and reopening the same database file without going through open_as_new_boot
// leaves owner_window and window bindings exactly as they were.

#[tokio::test]
async fn reopening_the_same_file_without_a_new_boot_preserves_owner_and_window_bindings() {
    let path = db_path("plan-store-plain-reopen");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let owner = open_window(&registry).await;
    let bound = open_window(&registry).await;

    let plan_id = {
        let mut store = PlanStore::open(&path).expect("PlanStore::open must succeed");
        let create_permit =
            plan_permit::issue_create(&RequestOrigin::Window(owner.clone()), &registry)
                .expect("the owner must be able to create a plan");
        let plan_id = store
            .create_plan(&create_permit, &one_task_plan("t1"), None)
            .expect("create_plan must succeed");

        let table = LockTable::new();
        let guard = table.lock(&plan_id).await;
        let bind_permit = plan_permit::issue_change(
            &RequestOrigin::Window(owner.clone()),
            ChangeKind::OpenTaskWindow,
            guard,
            &store,
            &registry,
        )
        .expect("the owner must be able to bind a task window");
        store
            .set_task_window(&bind_permit, "t1", &bound)
            .expect("set_task_window must succeed");
        plan_id
    };

    let store = PlanStore::open(&path).expect("a plain reopen of the same file must succeed");
    let detail = store
        .get_plan(&plan_id)
        .expect("get_plan must not error")
        .expect("the plan must survive a plain reopen");
    assert_eq!(
        detail.owner_window.as_deref(),
        Some(owner.as_str()),
        "a plain reopen (not a new boot) must keep owner_window"
    );
    assert_eq!(
        detail.tasks[0].window_id.as_deref(),
        Some(bound.as_str()),
        "a plain reopen must keep the task's window binding too"
    );
}

// Creation is bounded both overall (64) and per owner window (16); a window hitting its
// own cap does not block a different window, but the overall cap blocks every window
// once reached.

#[tokio::test]
async fn plan_creation_is_bounded_overall_and_per_owner_window() {
    let path = db_path("plan-store-limits");
    let mut store = PlanStore::open(&path).expect("PlanStore::open must succeed");
    let registry = WindowRegistry::new(DaemonConfig::default());

    // Four owners, each filling its own 16-plan cap: 4 x 16 = 64, the overall cap,
    // reached exactly as the fourth owner's cap is reached.
    let mut owners = Vec::new();
    for _ in 0..4 {
        owners.push(open_window(&registry).await);
    }

    for owner in &owners {
        for i in 0..16 {
            let permit =
                plan_permit::issue_create(&RequestOrigin::Window(owner.clone()), &registry)
                    .expect("the owner must be alive for every one of these creations");
            store
                .create_plan(&permit, &one_task_plan(&format!("t{i}")), None)
                .unwrap_or_else(|err| {
                    panic!("plan {i} for {owner} must succeed under the per-owner cap: {err:?}")
                });
        }
        let permit = plan_permit::issue_create(&RequestOrigin::Window(owner.clone()), &registry)
            .expect("the owner must still be alive for the overflow attempt");
        let overflow = store.create_plan(&permit, &one_task_plan("overflow"), None);
        assert!(
            matches!(
                overflow,
                Err(PlanStoreError::ResourceExhausted(
                    ResourceLimitKind::OwnerPlans
                ))
            ),
            "the 17th plan for one owner must be refused by the per-owner cap, got {overflow:?}"
        );
    }

    // A fifth owner with zero plans of its own must still be refused once the overall
    // cap has been reached by the other four -- being under one's own per-owner cap is
    // not enough.
    let fifth_owner = open_window(&registry).await;
    let permit = plan_permit::issue_create(&RequestOrigin::Window(fifth_owner), &registry)
        .expect("the fifth owner must be alive");
    let overflow_total = store.create_plan(&permit, &one_task_plan("overflow-total"), None);
    assert!(
        matches!(
            overflow_total,
            Err(PlanStoreError::ResourceExhausted(
                ResourceLimitKind::TotalPlans
            ))
        ),
        "a window with zero plans of its own must still be refused once the overall 64-plan \
         cap is reached, got {overflow_total:?}"
    );
}
