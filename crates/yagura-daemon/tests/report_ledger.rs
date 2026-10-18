#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Contracts for the task-report half of the plan ledger: `PlanStore`'s persisted report
//! columns (migration 008, schema-shape contracts for which live in
//! `tests/store_migrations.rs`) and `plan_permit`'s report authorization
//! (`plan_permit::issue_report`, `ReportPermit`) and parent derivation
//! (`plan_permit::resolve_owner_parent`).
//!
//! `issue_report` grants a [`ReportPermit`] only for the `task_id` a window is currently
//! bound to, read fresh at issuance: a report is never written
//! through a hand-built permit here, always through the real issuing function, the same
//! discipline `tests/plan_permit.rs` documents for every other permit kind.
//!
//! `resolve_owner_parent` is the "task-bound window -> plan owner, alive only" half of the
//! full parent-derivation rule; the opener fallback and the `task_origin`
//! binding check belong to the window-level `resolve_parent`, so this file
//! exercises `resolve_owner_parent` directly against a plan id rather than through any
//! window-opening entry point.
//!
//! Children are `echo_child` or `sleepy_command`, never a real vendor CLI, mirroring
//! `tests/plan_permit.rs`.

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::sleep;

use yagura_daemon::DaemonConfig;
use yagura_daemon::control::RequestOrigin;
use yagura_daemon::plan::{PlanSpec, TaskSpec, ValidatedPlan, validate_plan};
use yagura_daemon::plan_permit::{self, ChangeKind, LockTable, PermitError};
use yagura_daemon::plan_store::PlanStore;
use yagura_daemon::window::{OpenWindowRequest, WindowRegistry};
use yagura_protocol::{ReportOutcome, WindowStatus};
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

/// A window whose direct child stays alive for roughly two seconds and then exits on its
/// own, mirroring `tests/plan_permit.rs`'s fixture of the same purpose.
fn short_lived_request() -> OpenWindowRequest {
    OpenWindowRequest {
        command: sleepy_command("3"),
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
        .open(short_lived_request())
        .await
        .expect("open should succeed")
        .window_id
}

/// Polls a registry-held entry until it reports `Exited`; panics on timeout. Duplicated
/// from `tests/plan_permit.rs` rather than shared: each integration-test binary is its own
/// crate.
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

/// Two independent tasks (`t1`, `t2`), no dependency between them, distinct write targets
/// -- the same shape `tests/plan_store.rs::two_task_plan` uses.
fn two_task_plan() -> ValidatedPlan {
    let spec = PlanSpec {
        title: "report ledger fixture".to_string(),
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

/// A single-task plan naming `task_id`, valid against `validate_plan` with every profile id
/// accepted.
fn one_task_plan(task_id: &str) -> ValidatedPlan {
    let spec = PlanSpec {
        title: "report ledger single-task fixture".to_string(),
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

/// Opens a fresh store and registry, creates a two-task plan owned by a freshly opened
/// alive window, opens a second alive window and binds it to `t1`, and returns
/// `(store, registry, owner_window_id, reporter_window_id, plan_id)`. `t2` is left
/// deliberately unbound. The convenience every case below that needs an
/// existing task binding starts from.
async fn store_registry_owner_and_bound_reporter(
    tag: &str,
) -> (PlanStore, WindowRegistry, String, String, String) {
    let mut store = PlanStore::open(&db_path(tag)).expect("PlanStore::open must succeed");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let owner = open_window(&registry).await;
    let create_permit = plan_permit::issue_create(&RequestOrigin::Window(owner.clone()), &registry)
        .expect("a freshly opened window must be able to create a plan");
    let plan_id = store
        .create_plan(&create_permit, &two_task_plan(), None)
        .expect("create_plan must succeed for a valid two-task plan");

    let table = LockTable::new();
    let reporter = open_window(&registry).await;
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
        .set_task_window(&bind_permit, "t1", &reporter)
        .expect("set_task_window must succeed for the plan's own task");
    drop(bind_permit);

    (store, registry, owner, reporter, plan_id)
}

// --- Writing a report through a real ReportPermit lands exactly the report's 4
// columns against the reported task, leaves every other task's report untouched, and does
// not disturb the reporting task's own user override or window binding. ---

#[tokio::test]
async fn writing_a_report_updates_only_the_reported_tasks_report_columns() {
    let (mut store, registry, owner, reporter, plan_id) =
        store_registry_owner_and_bound_reporter("report-ledger-write").await;
    let table = LockTable::new();

    let override_guard = table.lock(&plan_id).await;
    let override_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner),
        ChangeKind::UpdatePlan,
        override_guard,
        &store,
        &registry,
    )
    .expect("the owner must hold a change permit for this plan");
    store
        .set_user_override(
            &override_permit,
            "t1",
            Some(yagura_daemon::plan::Assignment {
                profile: "claude-code".to_string(),
                model: None,
                effort: None,
            }),
        )
        .expect("set_user_override must succeed for t1");
    drop(override_permit);

    let report_guard = table.lock(&plan_id).await;
    let report_permit = plan_permit::issue_report(
        &RequestOrigin::Window(reporter.clone()),
        "t1",
        report_guard,
        &store,
    )
    .expect("the window bound to t1 must be authorized to report against it");
    store
        .write_task_report(
            &report_permit,
            ReportOutcome::Succeeded,
            "done",
            Some("out.txt"),
        )
        .expect("write_task_report must succeed for a validly issued permit");

    let detail = store
        .get_plan(&plan_id)
        .expect("get_plan must not error")
        .expect("the plan must still exist");
    let t1 = detail
        .tasks
        .iter()
        .find(|task| task.task_id == "t1")
        .expect("t1 must still be present");
    let t2 = detail
        .tasks
        .iter()
        .find(|task| task.task_id == "t2")
        .expect("t2 must still be present");

    let report = t1
        .report
        .as_ref()
        .expect("t1 must carry the report just written");
    assert_eq!(report.outcome, ReportOutcome::Succeeded);
    assert_eq!(report.summary, "done");
    assert_eq!(report.path.as_deref(), Some("out.txt"));
    assert!(
        report.reported_at > 0,
        "reported_at must be stamped with a positive epoch-seconds value, got {}",
        report.reported_at
    );

    assert_eq!(
        t2.report, None,
        "a task that received no report must still read back as having none"
    );
    assert_eq!(
        t1.user,
        Some(yagura_daemon::plan::Assignment {
            profile: "claude-code".to_string(),
            model: None,
            effort: None,
        }),
        "writing a report must not disturb the task's user override"
    );
    assert_eq!(
        t1.window_id,
        Some(reporter),
        "writing a report must not disturb the task's window binding"
    );
}

// --- A second report against the same task overwrites the first. ---

#[tokio::test]
async fn a_second_report_against_the_same_task_overwrites_the_first() {
    let (mut store, _registry, _owner, reporter, plan_id) =
        store_registry_owner_and_bound_reporter("report-ledger-overwrite").await;
    let table = LockTable::new();

    let first_guard = table.lock(&plan_id).await;
    let first_permit = plan_permit::issue_report(
        &RequestOrigin::Window(reporter.clone()),
        "t1",
        first_guard,
        &store,
    )
    .expect("the bound window must be authorized to report");
    store
        .write_task_report(
            &first_permit,
            ReportOutcome::Failed,
            "first attempt failed",
            None,
        )
        .expect("the first write_task_report must succeed");

    // `first_permit` owns the plan's lock guard; it must be released before
    // the same plan id is locked again, or the second `lock` call never
    // returns (a single-threaded owned-mutex self-deadlock).
    drop(first_permit);
    let second_guard = table.lock(&plan_id).await;
    let second_permit =
        plan_permit::issue_report(&RequestOrigin::Window(reporter), "t1", second_guard, &store)
            .expect("the same window may report again on the same task");
    store
        .write_task_report(
            &second_permit,
            ReportOutcome::Succeeded,
            "retried and succeeded",
            Some("result.txt"),
        )
        .expect("the second write_task_report must succeed");

    let detail = store
        .get_plan(&plan_id)
        .expect("get_plan must not error")
        .expect("the plan must still exist");
    let report = detail
        .tasks
        .iter()
        .find(|task| task.task_id == "t1")
        .and_then(|task| task.report.clone())
        .expect("t1 must carry a report after two writes");
    assert_eq!(report.outcome, ReportOutcome::Succeeded);
    assert_eq!(report.summary, "retried and succeeded");
    assert_eq!(report.path.as_deref(), Some("result.txt"));
}

// --- Rebinding a task's window (set_task_window) clears that task's report; an
// unrelated task's report is untouched. ---

#[tokio::test]
async fn rebinding_a_tasks_window_clears_that_tasks_report_but_not_others() {
    let (mut store, registry, owner, reporter, plan_id) =
        store_registry_owner_and_bound_reporter("report-ledger-rebind-clears").await;
    let table = LockTable::new();

    // Bind and report on t2 as well, so its report can be checked as untouched below.
    let bind_t2_guard = table.lock(&plan_id).await;
    let bind_t2_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner.clone()),
        ChangeKind::OpenTaskWindow,
        bind_t2_guard,
        &store,
        &registry,
    )
    .expect("the owner must be able to bind t2's window");
    let t2_reporter = open_window(&registry).await;
    store
        .set_task_window(&bind_t2_permit, "t2", &t2_reporter)
        .expect("set_task_window must succeed for t2");
    drop(bind_t2_permit);

    let report_t1_guard = table.lock(&plan_id).await;
    let report_t1_permit = plan_permit::issue_report(
        &RequestOrigin::Window(reporter),
        "t1",
        report_t1_guard,
        &store,
    )
    .expect("t1's bound window must be authorized to report");
    store
        .write_task_report(&report_t1_permit, ReportOutcome::Succeeded, "t1 done", None)
        .expect("write_task_report must succeed for t1");
    drop(report_t1_permit);

    let report_t2_guard = table.lock(&plan_id).await;
    let report_t2_permit = plan_permit::issue_report(
        &RequestOrigin::Window(t2_reporter),
        "t2",
        report_t2_guard,
        &store,
    )
    .expect("t2's bound window must be authorized to report");
    store
        .write_task_report(&report_t2_permit, ReportOutcome::Succeeded, "t2 done", None)
        .expect("write_task_report must succeed for t2");
    drop(report_t2_permit);

    // Rebind t1 to a brand-new window: this is a new attempt, so t1's old report must not
    // survive it.
    let rebind_guard = table.lock(&plan_id).await;
    let rebind_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner),
        ChangeKind::OpenTaskWindow,
        rebind_guard,
        &store,
        &registry,
    )
    .expect("the owner must be able to rebind t1's window");
    let new_t1_window = open_window(&registry).await;
    store
        .set_task_window(&rebind_permit, "t1", &new_t1_window)
        .expect("set_task_window must succeed for the rebind");

    let detail = store
        .get_plan(&plan_id)
        .expect("get_plan must not error")
        .expect("the plan must still exist");
    let t1 = detail
        .tasks
        .iter()
        .find(|task| task.task_id == "t1")
        .expect("t1 must still be present");
    let t2 = detail
        .tasks
        .iter()
        .find(|task| task.task_id == "t2")
        .expect("t2 must still be present");

    assert_eq!(
        t1.report, None,
        "rebinding t1's window must clear t1's report, since a new window means a new \
         attempt"
    );
    assert_eq!(
        t1.window_id,
        Some(new_t1_window),
        "the rebind itself must still take effect"
    );
    assert!(
        t2.report.is_some(),
        "rebinding t1's window must not clear t2's report"
    );
}

// --- replace_tasks carries a surviving task_id's report across; a removed
// task_id's report is discarded along with the rest of its row; a brand-new task_id starts
// with no report. ---

#[tokio::test]
async fn replacing_tasks_carries_over_a_survivors_report_and_drops_the_rest() {
    let (mut store, registry, owner, reporter, plan_id) =
        store_registry_owner_and_bound_reporter("report-ledger-replace").await;
    let table = LockTable::new();

    let bind_t2_guard = table.lock(&plan_id).await;
    let bind_t2_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner.clone()),
        ChangeKind::OpenTaskWindow,
        bind_t2_guard,
        &store,
        &registry,
    )
    .expect("the owner must be able to bind t2's window");
    let t2_reporter = open_window(&registry).await;
    store
        .set_task_window(&bind_t2_permit, "t2", &t2_reporter)
        .expect("set_task_window must succeed for t2");
    drop(bind_t2_permit);

    let report_t1_guard = table.lock(&plan_id).await;
    let report_t1_permit = plan_permit::issue_report(
        &RequestOrigin::Window(reporter),
        "t1",
        report_t1_guard,
        &store,
    )
    .expect("t1's bound window must be authorized to report");
    store
        .write_task_report(&report_t1_permit, ReportOutcome::Succeeded, "t1 done", None)
        .expect("write_task_report must succeed for t1");
    drop(report_t1_permit);

    let report_t2_guard = table.lock(&plan_id).await;
    let report_t2_permit = plan_permit::issue_report(
        &RequestOrigin::Window(t2_reporter),
        "t2",
        report_t2_guard,
        &store,
    )
    .expect("t2's bound window must be authorized to report");
    store
        .write_task_report(&report_t2_permit, ReportOutcome::Failed, "t2 failed", None)
        .expect("write_task_report must succeed for t2");
    drop(report_t2_permit);

    // Replace the plan's tasks with {t1, t3}: t1 survives, t2 is dropped, t3 is new.
    let replacement_spec = PlanSpec {
        title: "report ledger replacement".to_string(),
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
    assert_eq!(ids, vec!["t1", "t3"], "t2 must be gone after the replace");

    let t1 = detail
        .tasks
        .iter()
        .find(|task| task.task_id == "t1")
        .expect("t1 must survive the replace");
    let report = t1
        .report
        .as_ref()
        .expect("t1's report must survive a replace that keeps its task_id");
    assert_eq!(report.outcome, ReportOutcome::Succeeded);
    assert_eq!(report.summary, "t1 done");

    let t3 = detail
        .tasks
        .iter()
        .find(|task| task.task_id == "t3")
        .expect("t3 must be present as a brand-new task");
    assert_eq!(
        t3.report, None,
        "a brand-new task_id must start with no report"
    );
}

// --- Opening as a new boot clears window bindings but keeps a task's report. ---

#[tokio::test]
async fn opening_as_a_new_boot_keeps_a_tasks_report_even_though_its_binding_is_cleared() {
    let path = db_path("report-ledger-new-boot");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let owner = open_window(&registry).await;
    let reporter = open_window(&registry).await;

    let plan_id = {
        let mut store = PlanStore::open(&path).expect("PlanStore::open must succeed");
        let create_permit =
            plan_permit::issue_create(&RequestOrigin::Window(owner.clone()), &registry)
                .expect("the owner must be able to create a plan");
        let plan_id = store
            .create_plan(&create_permit, &one_task_plan("t1"), None)
            .expect("create_plan must succeed");

        let table = LockTable::new();
        let bind_guard = table.lock(&plan_id).await;
        let bind_permit = plan_permit::issue_change(
            &RequestOrigin::Window(owner),
            ChangeKind::OpenTaskWindow,
            bind_guard,
            &store,
            &registry,
        )
        .expect("the owner must be able to bind t1's window");
        store
            .set_task_window(&bind_permit, "t1", &reporter)
            .expect("set_task_window must succeed");
        drop(bind_permit);

        let report_guard = table.lock(&plan_id).await;
        let report_permit =
            plan_permit::issue_report(&RequestOrigin::Window(reporter), "t1", report_guard, &store)
                .expect("the bound window must be authorized to report");
        store
            .write_task_report(
                &report_permit,
                ReportOutcome::Succeeded,
                "done before restart",
                None,
            )
            .expect("write_task_report must succeed");
        plan_id
    };

    let store = PlanStore::open_as_new_boot(&path).expect("open_as_new_boot must succeed");
    let detail = store
        .get_plan(&plan_id)
        .expect("get_plan must not error")
        .expect("the plan must survive a new boot");
    let t1 = &detail.tasks[0];
    assert_eq!(
        t1.window_id, None,
        "a new boot must still clear the task's window binding"
    );
    let report = t1
        .report
        .as_ref()
        .expect("a new boot must keep the task's report even though its binding is cleared");
    assert_eq!(report.summary, "done before restart");
}

// --- A report permit is granted only for a task_id the reporting window is
// currently bound to. Refused for: a task never bound to any window, an unrelated window
// reporting on a task bound to someone else, a task_id the plan does not have, a window
// whose binding was since replaced, and a task_id that was since removed entirely. ---

#[tokio::test]
async fn report_authorization_requires_a_current_task_binding_to_the_reporting_window() {
    let (mut store, registry, owner, reporter, plan_id) =
        store_registry_owner_and_bound_reporter("report-ledger-authz-binding").await;
    let table = LockTable::new();

    let ok_guard = table.lock(&plan_id).await;
    assert!(
        plan_permit::issue_report(
            &RequestOrigin::Window(reporter.clone()),
            "t1",
            ok_guard,
            &store
        )
        .is_ok(),
        "the window currently bound to t1 must be authorized to report against it"
    );

    let unbound_guard = table.lock(&plan_id).await;
    let unbound_result = plan_permit::issue_report(
        &RequestOrigin::Window(reporter.clone()),
        "t2",
        unbound_guard,
        &store,
    );
    assert!(
        matches!(unbound_result, Err(PermitError::InvalidState)),
        "a task with no window binding at all must be refused invalid_state, got \
         {unbound_result:?}"
    );

    let unrelated = open_window(&registry).await;
    let unrelated_guard = table.lock(&plan_id).await;
    let unrelated_result = plan_permit::issue_report(
        &RequestOrigin::Window(unrelated),
        "t1",
        unrelated_guard,
        &store,
    );
    assert!(
        matches!(unrelated_result, Err(PermitError::InvalidState)),
        "a window not bound to t1 must be refused invalid_state, got {unrelated_result:?}"
    );

    let missing_guard = table.lock(&plan_id).await;
    let missing_result = plan_permit::issue_report(
        &RequestOrigin::Window(reporter.clone()),
        "does-not-exist",
        missing_guard,
        &store,
    );
    assert!(
        matches!(missing_result, Err(PermitError::InvalidState)),
        "a task_id this plan does not have must be refused invalid_state, got {missing_result:?}"
    );

    let rebind_target = open_window(&registry).await;
    let rebind_guard = table.lock(&plan_id).await;
    let rebind_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner.clone()),
        ChangeKind::OpenTaskWindow,
        rebind_guard,
        &store,
        &registry,
    )
    .expect("the owner must be able to rebind t1's window");
    store
        .set_task_window(&rebind_permit, "t1", &rebind_target)
        .expect("set_task_window must succeed for the rebind");
    drop(rebind_permit);

    let stale_guard = table.lock(&plan_id).await;
    let stale_result =
        plan_permit::issue_report(&RequestOrigin::Window(reporter), "t1", stale_guard, &store);
    assert!(
        matches!(stale_result, Err(PermitError::InvalidState)),
        "a window whose task binding was replaced must be refused invalid_state for its old \
         task_id, got {stale_result:?}"
    );

    let new_binding_guard = table.lock(&plan_id).await;
    assert!(
        plan_permit::issue_report(
            &RequestOrigin::Window(rebind_target.clone()),
            "t1",
            new_binding_guard,
            &store
        )
        .is_ok(),
        "the newly bound window must be authorized to report against t1"
    );

    let deleted_task_plan = one_task_plan("t2-only");
    let delete_guard = table.lock(&plan_id).await;
    let delete_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner),
        ChangeKind::UpdatePlan,
        delete_guard,
        &store,
        &registry,
    )
    .expect("the owner must be authorized to replace the plan's tasks");
    store
        .replace_tasks(&delete_permit, &deleted_task_plan)
        .expect("replace_tasks must succeed");
    drop(delete_permit);

    let after_delete_guard = table.lock(&plan_id).await;
    let after_delete_result = plan_permit::issue_report(
        &RequestOrigin::Window(rebind_target),
        "t1",
        after_delete_guard,
        &store,
    );
    assert!(
        matches!(after_delete_result, Err(PermitError::InvalidState)),
        "reporting against a task_id that no longer exists must be refused invalid_state, \
         got {after_delete_result:?}"
    );
}

// --- External and Approver origins are never authorized to report, whatever the
// task_id names. ---

#[tokio::test]
async fn report_authorization_rejects_external_and_approver_origins() {
    let (store, _registry, _owner, _reporter, plan_id) =
        store_registry_owner_and_bound_reporter("report-ledger-authz-origin").await;
    let table = LockTable::new();

    let external_guard = table.lock(&plan_id).await;
    let external_result =
        plan_permit::issue_report(&RequestOrigin::External, "t1", external_guard, &store);
    assert!(
        matches!(external_result, Err(PermitError::AuthFailed)),
        "an unidentified connection must never be authorized to report, got {external_result:?}"
    );

    let approver_guard = table.lock(&plan_id).await;
    let approver_result =
        plan_permit::issue_report(&RequestOrigin::Approver, "t1", approver_guard, &store);
    assert!(
        matches!(approver_result, Err(PermitError::AuthFailed)),
        "an approver connection must never report on a window's behalf, got {approver_result:?}"
    );
}

// --- Parent derivation follows the plan owner's liveness, and tracks a
// nomination. ---

#[tokio::test]
async fn parent_derivation_follows_the_owners_liveness_and_nomination() {
    let path = db_path("report-ledger-parent");
    let mut store = PlanStore::open(&path).expect("PlanStore::open must succeed");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let owner = open_window(&registry).await;

    let create_permit = plan_permit::issue_create(&RequestOrigin::Window(owner.clone()), &registry)
        .expect("the owner must be able to create a plan");
    let plan_id = store
        .create_plan(&create_permit, &one_task_plan("t1"), None)
        .expect("create_plan must succeed");

    assert_eq!(
        plan_permit::resolve_owner_parent(&store, &registry, &plan_id),
        Some(owner),
        "a plan's alive owner must resolve as the parent"
    );

    let table = LockTable::new();
    let short_lived = open_short_lived_window(&registry).await;
    let nominate_guard = table.lock(&plan_id).await;
    let nominate_permit = plan_permit::issue_nominate(
        &RequestOrigin::Approver,
        nominate_guard,
        &short_lived,
        &registry,
    )
    .expect("an approver must be able to nominate an alive window as owner");
    store
        .set_owner(nominate_permit)
        .expect("set_owner must persist the nominated owner");

    assert_eq!(
        plan_permit::resolve_owner_parent(&store, &registry, &plan_id),
        Some(short_lived.clone()),
        "once nominated and still alive, the new owner must resolve as the parent"
    );

    wait_for_exited(&registry, &short_lived).await;

    assert_eq!(
        plan_permit::resolve_owner_parent(&store, &registry, &plan_id),
        None,
        "an exited owner must resolve to no parent, even though the plan still names it -- \
         the report permit itself is unaffected by this (the permit does not check owner \
         liveness at all)"
    );
}

// --- A ReportPermit moved into a spawn_blocking closure still licenses the write
// it was issued for (the door StoreGate::with_store -- src/ipc/chat.rs -- requires
// F: Send + 'static, so this is the shape production code must compile). ---

#[tokio::test]
async fn a_report_permit_moved_into_spawn_blocking_still_licenses_a_store_write() {
    let (store, _registry, _owner, reporter, plan_id) =
        store_registry_owner_and_bound_reporter("report-ledger-spawn-blocking").await;
    let table = LockTable::new();

    let guard = table.lock(&plan_id).await;
    let permit = plan_permit::issue_report(&RequestOrigin::Window(reporter), "t1", guard, &store)
        .expect("the bound window must be authorized to report");

    let store = Arc::new(Mutex::new(store));
    let store_in_closure = store.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let mut guard = store_in_closure
            .lock()
            .expect("the store mutex must not be poisoned");
        guard.write_task_report(
            &permit,
            ReportOutcome::Succeeded,
            "done from spawn_blocking",
            None,
        )
    })
    .await
    .expect("the blocking task must not panic or be cancelled");

    assert!(
        outcome.is_ok(),
        "a ReportPermit moved into a spawn_blocking closure must still license the write it \
         was issued for, got {outcome:?}"
    );

    let store = store.lock().expect("the store mutex must not be poisoned");
    let detail = store
        .get_plan(&plan_id)
        .expect("get_plan must not error")
        .expect("the plan must still exist");
    let report = detail
        .tasks
        .iter()
        .find(|task| task.task_id == "t1")
        .and_then(|task| task.report.clone())
        .expect("the write issued through the moved permit must actually have landed");
    assert_eq!(report.summary, "done from spawn_blocking");
}
