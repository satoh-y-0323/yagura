#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Seeds and reads the database with raw SQL, which only tests may do.
#![allow(clippy::disallowed_methods)]

//! Contracts for the plan ledger's review state: whether a plan waits for the
//! operator (`PlanDetail::review`), which revision of the task list the operator
//! looked at (`PlanDetail::version`), and which tasks changed since the operator
//! last looked (`PlanTaskRecord::review_mark`, `PlanSummary::unseen_changes`).
//!
//! Every write goes through a permit from the real issuing functions in
//! `plan_permit`; `tests/plan_review_permit.rs` is the authorization contract for
//! those permits.

mod common;

use std::path::{Path, PathBuf};

use rusqlite::params;

use yagura_daemon::DaemonConfig;
use yagura_daemon::control::RequestOrigin;
use yagura_daemon::plan::{Assignment, PlanSpec, TaskSpec, ValidatedPlan, validate_plan};
use yagura_daemon::plan_permit::{self, ChangeKind, LockTable};
use yagura_daemon::plan_store::{
    ApproveOutcome, ClearOutcome, PlanDetail, PlanReview, PlanStore, PlanSummary, ReviewMark,
};
use yagura_daemon::store;
use yagura_daemon::window::{OpenWindowRequest, WindowRegistry};
use yagura_protocol::ReportOutcome;
use yagura_test_support::process::echo_child_path;

/// The schema version every migration up to and including the snapshot-profile one
/// reaches: the last version before the plan ledger learned about review state.
const SCHEMA_BEFORE_REVIEW: usize = 11;

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

async fn open_window(registry: &WindowRegistry) -> String {
    registry
        .open(echo_request())
        .await
        .expect("open should succeed")
        .window_id
}

fn task(task_id: &str) -> TaskSpec {
    TaskSpec {
        task_id: task_id.to_string(),
        title: format!("{task_id} title"),
        role: None,
        profile: Some("codex-cli".to_string()),
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: None,
    }
}

fn plan_of(tasks: Vec<TaskSpec>) -> ValidatedPlan {
    let spec = PlanSpec {
        title: "review fixture".to_string(),
        tasks,
    };
    validate_plan(&spec, |_| true).expect("the fixture plan must validate")
}

/// One task per attribute a change is judged on, plus one that never changes.
fn attribute_tasks() -> Vec<TaskSpec> {
    [
        "keep",
        "by-title",
        "by-role",
        "by-parent",
        "by-deps",
        "by-writes",
        "by-instruction",
        "doomed",
    ]
    .into_iter()
    .map(task)
    .collect()
}

/// `attribute_tasks` with exactly one attribute changed on each `by-*` task, `doomed`
/// removed and `fresh` added.
fn attribute_tasks_changed() -> Vec<TaskSpec> {
    let mut tasks: Vec<TaskSpec> = attribute_tasks()
        .into_iter()
        .filter(|t| t.task_id != "doomed")
        .collect();
    for t in &mut tasks {
        match t.task_id.as_str() {
            "by-title" => t.title = "a retitled task".to_string(),
            "by-role" => t.role = Some("impl".to_string()),
            "by-parent" => t.model = Some("opus".to_string()),
            "by-deps" => t.depends_on = vec!["keep".to_string()],
            "by-writes" => t.writes = vec!["elsewhere.txt".to_string()],
            "by-instruction" => t.instruction = Some("read the spec first".to_string()),
            _ => {}
        }
    }
    tasks.push(task("fresh"));
    tasks
}

struct Fixture {
    store: PlanStore,
    registry: WindowRegistry,
    table: LockTable,
    owner: String,
    path: PathBuf,
}

async fn fixture(tag: &str) -> Fixture {
    let path = db_path(tag);
    let store = PlanStore::open(&path).expect("PlanStore::open must succeed");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let owner = open_window(&registry).await;
    Fixture {
        store,
        registry,
        table: LockTable::new(),
        owner,
        path,
    }
}

impl Fixture {
    fn create(&mut self, plan: &ValidatedPlan, review: Option<PlanReview>) -> String {
        let permit =
            plan_permit::issue_create(&RequestOrigin::Window(self.owner.clone()), &self.registry)
                .expect("the alive owner must be able to create a plan");
        self.store
            .create_plan(&permit, plan, review)
            .expect("create_plan must succeed for a valid plan")
    }

    async fn replace(&mut self, plan_id: &str, plan: &ValidatedPlan) {
        let guard = self.table.lock(plan_id).await;
        let permit = plan_permit::issue_change(
            &RequestOrigin::Window(self.owner.clone()),
            ChangeKind::UpdatePlan,
            guard,
            &self.store,
            &self.registry,
        )
        .expect("the alive owner must be able to update its plan");
        self.store
            .replace_tasks(&permit, plan)
            .expect("replace_tasks must succeed");
    }

    async fn approve(&mut self, plan_id: &str, version: u64) -> ApproveOutcome {
        let guard = self.table.lock(plan_id).await;
        let permit = plan_permit::issue_change(
            &RequestOrigin::Approver,
            ChangeKind::ApprovePlan,
            guard,
            &self.store,
            &self.registry,
        )
        .expect("the approver must be able to approve a plan");
        self.store
            .approve_plan(&permit, version)
            .expect("approve_plan must not fail on storage")
    }

    async fn clear_marks(&mut self, plan_id: &str, version: u64) -> ClearOutcome {
        let guard = self.table.lock(plan_id).await;
        let permit = plan_permit::issue_change(
            &RequestOrigin::Approver,
            ChangeKind::MarkPlanSeen,
            guard,
            &self.store,
            &self.registry,
        )
        .expect("the approver must be able to mark a plan seen");
        self.store
            .clear_review_marks(&permit, version)
            .expect("clear_review_marks must not fail on storage")
    }

    /// Creates a pending plan and approves it at its first version.
    async fn create_approved(&mut self, plan: &ValidatedPlan) -> String {
        let plan_id = self.create(plan, Some(PlanReview::Pending));
        let outcome = self.approve(&plan_id, 0).await;
        assert!(
            matches!(outcome, ApproveOutcome::Approved),
            "a pending plan at its current version must be approvable, got {outcome:?}"
        );
        plan_id
    }

    fn detail(&self, plan_id: &str) -> PlanDetail {
        self.store
            .get_plan(plan_id)
            .expect("get_plan must not error")
            .expect("the plan must exist")
    }

    fn summary(&self, plan_id: &str) -> PlanSummary {
        self.store
            .list_plans()
            .expect("list_plans must not error")
            .into_iter()
            .find(|s| s.plan_id == plan_id)
            .expect("the plan must be listed")
    }
}

fn mark_of<'a>(detail: &'a PlanDetail, task_id: &str) -> Option<&'a ReviewMark> {
    detail
        .tasks
        .iter()
        .find(|t| t.task_id == task_id)
        .unwrap_or_else(|| panic!("task {task_id} must exist"))
        .review_mark
        .as_ref()
}

fn user_version(path: &Path) -> i64 {
    let conn = rusqlite::Connection::open(path).expect("open the plan database directly");
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("user_version must be readable")
}

#[tokio::test]
async fn a_database_from_before_review_state_migrates_forward_with_plans_left_unreviewed() {
    let path = db_path("plan-review-legacy");
    {
        let mut conn = rusqlite::Connection::open(&path).expect("open a fresh sqlite connection");
        store::apply_migrations(&mut conn, &store::MIGRATIONS[..SCHEMA_BEFORE_REVIEW])
            .expect("every migration before review state must apply");
        conn.execute(
            "INSERT INTO plan (plan_id, title, cwd, owner_window, created_at, updated_at) \
             VALUES (?1, ?2, ?3, NULL, 0, 0)",
            params!["p-legacy", "legacy plan", "C:/work"],
        )
        .expect("seed a plan written before review state existed");
        conn.execute(
            "INSERT INTO plan_task (plan_id, task_id, seq, title) VALUES (?1, ?2, 0, ?3)",
            params!["p-legacy", "t1", "legacy task"],
        )
        .expect("seed a task written before review state existed");
    }

    let store = PlanStore::open(&path).expect("PlanStore::open must migrate the old database");
    let version = user_version(&path);
    assert!(
        version > SCHEMA_BEFORE_REVIEW as i64,
        "opening a database from before review state must migrate it forward, got \
         user_version {version}"
    );
    assert_eq!(version, store::MIGRATIONS.len() as i64);

    let detail = store
        .get_plan("p-legacy")
        .expect("get_plan must not error")
        .expect("the legacy plan must survive the migration");
    assert_eq!(
        detail.review, None,
        "a plan that predates review state must not wait for the operator"
    );
    assert_eq!(detail.version, 0);
    assert_eq!(detail.tasks[0].review_mark, None);

    let summary = store
        .list_plans()
        .expect("list_plans must not error")
        .into_iter()
        .find(|s| s.plan_id == "p-legacy")
        .expect("the legacy plan must be listed");
    assert_eq!(summary.review, None);
    assert!(!summary.unseen_changes);
}

#[tokio::test]
async fn create_plan_stores_the_review_state_it_was_given() {
    let mut f = fixture("plan-review-create").await;
    let pending = f.create(&plan_of(vec![task("t1")]), Some(PlanReview::Pending));
    let unreviewed = f.create(&plan_of(vec![task("t1")]), None);

    assert_eq!(f.detail(&pending).review, Some(PlanReview::Pending));
    assert_eq!(f.detail(&pending).version, 0);
    assert_eq!(f.summary(&pending).review, Some(PlanReview::Pending));

    assert_eq!(f.detail(&unreviewed).review, None);
    assert_eq!(f.detail(&unreviewed).version, 0);
    assert_eq!(f.summary(&unreviewed).review, None);
}

#[tokio::test]
async fn replacing_an_approved_plans_tasks_marks_every_added_or_changed_task() {
    let mut f = fixture("plan-review-marks").await;
    let plan_id = f.create_approved(&plan_of(attribute_tasks())).await;

    f.replace(&plan_id, &plan_of(attribute_tasks_changed()))
        .await;
    let detail = f.detail(&plan_id);

    assert_eq!(
        mark_of(&detail, "keep"),
        None,
        "an unchanged task must not be marked"
    );
    for changed in [
        "by-title",
        "by-role",
        "by-parent",
        "by-deps",
        "by-writes",
        "by-instruction",
    ] {
        assert_eq!(
            mark_of(&detail, changed),
            Some(&ReviewMark::Changed),
            "{changed} differs from its previous version in exactly one attribute and must be \
             marked changed"
        );
    }
    assert_eq!(mark_of(&detail, "fresh"), Some(&ReviewMark::New));
    assert!(
        detail.tasks.iter().all(|t| t.task_id != "doomed"),
        "a removed task must not come back"
    );
    assert_eq!(detail.review, Some(PlanReview::Approved));
    assert!(f.summary(&plan_id).unseen_changes);
}

#[tokio::test]
async fn a_later_replace_carries_marks_over_and_a_new_task_stays_new_when_changed() {
    let mut f = fixture("plan-review-carry").await;
    let plan_id = f.create_approved(&plan_of(attribute_tasks())).await;
    f.replace(&plan_id, &plan_of(attribute_tasks_changed()))
        .await;

    // `fresh` changes while still new, `keep` changes for the first time, every
    // other task is resubmitted exactly as before.
    let mut next = attribute_tasks_changed();
    for t in &mut next {
        match t.task_id.as_str() {
            "fresh" => t.title = "a retitled new task".to_string(),
            "keep" => t.writes = vec!["keep-moved.txt".to_string()],
            _ => {}
        }
    }
    f.replace(&plan_id, &plan_of(next)).await;
    let detail = f.detail(&plan_id);

    assert_eq!(
        mark_of(&detail, "fresh"),
        Some(&ReviewMark::New),
        "a task the operator has not seen yet must stay new when it changes again"
    );
    assert_eq!(mark_of(&detail, "keep"), Some(&ReviewMark::Changed));
    assert_eq!(
        mark_of(&detail, "by-title"),
        Some(&ReviewMark::Changed),
        "an unchanged resubmission must carry the previous mark over"
    );
}

#[tokio::test]
async fn removing_the_only_marked_task_leaves_no_unseen_change() {
    let mut f = fixture("plan-review-remove").await;
    let plan_id = f.create_approved(&plan_of(vec![task("t1")])).await;

    f.replace(&plan_id, &plan_of(vec![task("t1"), task("t2")]))
        .await;
    assert_eq!(mark_of(&f.detail(&plan_id), "t2"), Some(&ReviewMark::New));
    assert!(f.summary(&plan_id).unseen_changes);

    f.replace(&plan_id, &plan_of(vec![task("t1")])).await;
    let detail = f.detail(&plan_id);
    assert_eq!(detail.tasks.len(), 1);
    assert_eq!(mark_of(&detail, "t1"), None);
    assert!(
        !f.summary(&plan_id).unseen_changes,
        "a removed task's mark must go with it"
    );
}

#[tokio::test]
async fn replacing_the_tasks_of_a_pending_or_unreviewed_plan_marks_nothing() {
    let mut f = fixture("plan-review-no-marks").await;
    let pending = f.create(&plan_of(attribute_tasks()), Some(PlanReview::Pending));
    let unreviewed = f.create(&plan_of(attribute_tasks()), None);

    for plan_id in [&pending, &unreviewed] {
        f.replace(plan_id, &plan_of(attribute_tasks_changed()))
            .await;
        let detail = f.detail(plan_id);
        assert!(
            detail.tasks.iter().all(|t| t.review_mark.is_none()),
            "only an approved plan's changes are marked, got {:?}",
            detail
                .tasks
                .iter()
                .map(|t| (&t.task_id, &t.review_mark))
                .collect::<Vec<_>>()
        );
        assert!(!f.summary(plan_id).unseen_changes);
    }
    assert_eq!(f.detail(&pending).review, Some(PlanReview::Pending));
    assert_eq!(f.detail(&unreviewed).review, None);
}

#[tokio::test]
async fn the_version_advances_on_every_replace_and_nowhere_else() {
    let mut f = fixture("plan-review-version").await;
    let reporter_bound = open_window(&f.registry).await;
    let plan_id = f
        .create_approved(&plan_of(vec![task("t1"), task("t2")]))
        .await;
    assert_eq!(
        f.detail(&plan_id).version,
        0,
        "approving must not advance the version"
    );

    f.replace(&plan_id, &plan_of(vec![task("t1"), task("t2"), task("t3")]))
        .await;
    assert_eq!(f.detail(&plan_id).version, 1);
    f.replace(&plan_id, &plan_of(vec![task("t1"), task("t2"), task("t3")]))
        .await;
    assert_eq!(
        f.detail(&plan_id).version,
        2,
        "every replace advances the version, even one that changes nothing"
    );
    let before = f.detail(&plan_id);
    assert_eq!(mark_of(&before, "t3"), Some(&ReviewMark::New));

    // User override on an unmarked task.
    let guard = f.table.lock(&plan_id).await;
    let permit = plan_permit::issue_change(
        &RequestOrigin::Approver,
        ChangeKind::SetTaskAssignment,
        guard,
        &f.store,
        &f.registry,
    )
    .expect("the approver must be able to set an override");
    f.store
        .set_user_override(
            &permit,
            "t1",
            Some(Assignment {
                profile: "claude-code".to_string(),
                model: None,
                effort: None,
            }),
        )
        .expect("set_user_override must succeed");
    drop(permit);

    // Window binding on the marked task.
    let guard = f.table.lock(&plan_id).await;
    let permit = plan_permit::issue_change(
        &RequestOrigin::Window(f.owner.clone()),
        ChangeKind::OpenTaskWindow,
        guard,
        &f.store,
        &f.registry,
    )
    .expect("the alive owner of an approved plan must be able to bind a task window");
    f.store
        .set_task_window(&permit, "t3", &reporter_bound)
        .expect("set_task_window must succeed");
    drop(permit);

    // A report from the bound window.
    let guard = f.table.lock(&plan_id).await;
    let permit = plan_permit::issue_report(
        &RequestOrigin::Window(reporter_bound.clone()),
        "t3",
        guard,
        &f.store,
    )
    .expect("the bound window must be able to report");
    f.store
        .write_task_report(&permit, ReportOutcome::Succeeded, "done", None)
        .expect("write_task_report must succeed");
    drop(permit);

    let after = f.detail(&plan_id);
    assert_eq!(
        after.version, 2,
        "an override, a window binding and a report must not advance the version"
    );
    for t in &before.tasks {
        assert_eq!(
            mark_of(&after, &t.task_id),
            t.review_mark.as_ref(),
            "an override, a window binding and a report must not touch {}'s mark",
            t.task_id
        );
    }
}

#[tokio::test]
async fn approving_moves_only_a_pending_plan_at_its_current_version_to_approved() {
    let mut f = fixture("plan-review-approve").await;

    let pending = f.create(&plan_of(vec![task("t1")]), Some(PlanReview::Pending));
    f.replace(&pending, &plan_of(vec![task("t1"), task("t2")]))
        .await;
    let stale = f.approve(&pending, 0).await;
    assert!(
        matches!(stale, ApproveOutcome::VersionMismatch),
        "approving a version the plan has moved past must be refused, got {stale:?}"
    );
    assert_eq!(f.detail(&pending).review, Some(PlanReview::Pending));

    let current = f.approve(&pending, 1).await;
    assert!(
        matches!(current, ApproveOutcome::Approved),
        "approving the current version must succeed, got {current:?}"
    );
    assert_eq!(f.detail(&pending).review, Some(PlanReview::Approved));
    assert_eq!(f.summary(&pending).review, Some(PlanReview::Approved));

    let again = f.approve(&pending, 1).await;
    assert!(
        matches!(again, ApproveOutcome::AlreadyApproved),
        "approving an approved plan must report that it already was, got {again:?}"
    );
    assert_eq!(f.detail(&pending).review, Some(PlanReview::Approved));

    let unreviewed = f.create(&plan_of(vec![task("t1")]), None);
    let not_reviewable = f.approve(&unreviewed, 0).await;
    assert!(
        matches!(not_reviewable, ApproveOutcome::NotReviewable),
        "a plan created without review has nothing to approve, got {not_reviewable:?}"
    );
    assert_eq!(f.detail(&unreviewed).review, None);
}

#[tokio::test]
async fn approving_or_clearing_a_plan_that_no_longer_exists_reports_it_gone() {
    let mut f = fixture("plan-review-gone").await;

    let never = f.approve("p-never-existed", 0).await;
    assert!(matches!(never, ApproveOutcome::PlanGone), "got {never:?}");
    let never = f.clear_marks("p-never-existed", 0).await;
    assert!(matches!(never, ClearOutcome::PlanGone), "got {never:?}");

    let plan_id = f.create(&plan_of(vec![task("t1")]), Some(PlanReview::Pending));
    let guard = f.table.lock(&plan_id).await;
    let permit = plan_permit::issue_change(
        &RequestOrigin::Approver,
        ChangeKind::DeletePlan,
        guard,
        &f.store,
        &f.registry,
    )
    .expect("the approver must be able to delete a plan");
    f.store
        .delete_plan(permit)
        .expect("delete_plan must succeed");

    let deleted = f.approve(&plan_id, 0).await;
    assert!(
        matches!(deleted, ApproveOutcome::PlanGone),
        "a plan deleted after its permit could be issued must be reported gone, got {deleted:?}"
    );
    let deleted = f.clear_marks(&plan_id, 0).await;
    assert!(matches!(deleted, ClearOutcome::PlanGone), "got {deleted:?}");
}

#[tokio::test]
async fn clearing_marks_applies_only_at_the_current_version() {
    let mut f = fixture("plan-review-clear").await;
    let plan_id = f.create_approved(&plan_of(attribute_tasks())).await;
    let untouched = f.create_approved(&plan_of(vec![task("t1")])).await;

    f.replace(&plan_id, &plan_of(attribute_tasks_changed()))
        .await;
    let marked = f.detail(&plan_id);
    let summary = f.summary(&plan_id);
    assert!(summary.unseen_changes);
    assert_eq!(summary.review, Some(PlanReview::Approved));
    assert!(
        !f.summary(&untouched).unseen_changes,
        "another plan's marks must not show on this one's summary"
    );

    let stale = f.clear_marks(&plan_id, 0).await;
    assert!(
        matches!(stale, ClearOutcome::VersionMismatch),
        "clearing at a version the plan has moved past must be refused, got {stale:?}"
    );
    let still = f.detail(&plan_id);
    for t in &marked.tasks {
        assert_eq!(
            mark_of(&still, &t.task_id),
            t.review_mark.as_ref(),
            "a refused clear must leave {}'s mark in place",
            t.task_id
        );
    }
    assert!(f.summary(&plan_id).unseen_changes);

    let cleared = f.clear_marks(&plan_id, 1).await;
    assert!(
        matches!(cleared, ClearOutcome::Cleared),
        "clearing at the current version must succeed, got {cleared:?}"
    );
    let detail = f.detail(&plan_id);
    assert!(detail.tasks.iter().all(|t| t.review_mark.is_none()));
    assert_eq!(
        detail.version, 1,
        "clearing marks must not advance the version"
    );
    assert!(!f.summary(&plan_id).unseen_changes);
}

#[tokio::test]
async fn review_state_version_and_marks_survive_a_new_boot() {
    let mut f = fixture("plan-review-new-boot").await;
    let pending = f.create(&plan_of(vec![task("t1")]), Some(PlanReview::Pending));
    let approved = f.create_approved(&plan_of(vec![task("t1")])).await;
    f.replace(&approved, &plan_of(vec![task("t1"), task("t2")]))
        .await;
    let path = f.path.clone();
    drop(f);

    let store = PlanStore::open_as_new_boot(&path).expect("open_as_new_boot must succeed");
    let pending_detail = store.get_plan(&pending).unwrap().unwrap();
    assert_eq!(pending_detail.review, Some(PlanReview::Pending));

    let approved_detail = store.get_plan(&approved).unwrap().unwrap();
    assert_eq!(approved_detail.review, Some(PlanReview::Approved));
    assert_eq!(approved_detail.version, 1);
    assert_eq!(mark_of(&approved_detail, "t2"), Some(&ReviewMark::New));
}
