#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Contracts for the instruction-notice half of the plan ledger: `PlanStore`'s persisted
//! delivery-state columns (migration 010, schema-shape contracts for which live in
//! `tests/store_migrations.rs`), `PlanStore::write_instruction_notice`, and
//! `plan_permit`'s notice authorization (`plan_permit::issue_notice`, `NoticePermit`).
//!
//! This file mirrors `tests/report_ledger.rs`'s split between store-level effects
//! and permit authorization -- a permit tied to one
//! ledger feature gets its own authorization contract colocated with that feature rather
//! than folded into `tests/plan_permit.rs`, which only covers the permit kinds every plan
//! feature shares (`CreatePermit`, `ChangePermit`, `ReadPermit`, `NominatePermit`).
//!
//! Unlike `issue_report`, `issue_notice` is never granted from a `RequestOrigin`: the
//! daemon's own notice-delivery job holds no connection to authenticate,
//! only the `window_id` it is currently trying to deliver to. It still reads the task's
//! *current* binding fresh from the store at the moment of the call (mirroring
//! `check_task_binding`), so a delivery job holding a
//! rebind-stale `window_id` cannot license a write against the task's new binding:
//! a notice is never written through a hand-built permit here, always
//! through the real issuing function.
//!
//! Children are `echo_child`, never a real vendor CLI, mirroring `tests/plan_permit.rs`
//! and `tests/report_ledger.rs`.

mod common;

use std::path::PathBuf;

use yagura_daemon::DaemonConfig;
use yagura_daemon::control::RequestOrigin;
use yagura_daemon::plan::{PlanSpec, TaskSpec, ValidatedPlan, validate_plan};
use yagura_daemon::plan_permit::{self, ChangeKind, LockTable, PermitError};
use yagura_daemon::plan_store::PlanStore;
use yagura_daemon::window::{OpenWindowRequest, WindowRegistry};
use yagura_protocol::InstructionNoticeState;
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

/// A single task, `instruction` set as given.
fn task_spec(task_id: &str, instruction: Option<&str>) -> TaskSpec {
    TaskSpec {
        task_id: task_id.to_string(),
        title: format!("task {task_id}"),
        role: None,
        profile: Some("codex-cli".to_string()),
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: instruction.map(str::to_string),
    }
}

/// Two independent tasks, both carrying an instruction: `t1` and `t2`, distinct write
/// targets, no dependency between them -- the isolation fixture the store-level
/// cases below start from.
fn two_task_plan_with_instructions() -> ValidatedPlan {
    let spec = PlanSpec {
        title: "instruction notice ledger fixture".to_string(),
        tasks: vec![
            task_spec("t1", Some("investigate the failing gate")),
            task_spec("t2", Some("summarize the change")),
        ],
    };
    validate_plan(&spec, |_| true).expect("a two-task plan with no cycles must validate")
}

/// One task carrying an instruction (`t1`), one carrying none (`t2`) -- the fixture
/// the binding and new-boot cases need to tell "binding never creates a notice for an
/// instruction-less task" apart from "binding sets a pending one".
fn task_plan_one_with_and_one_without_instruction() -> ValidatedPlan {
    let spec = PlanSpec {
        title: "instruction notice ledger mixed fixture".to_string(),
        tasks: vec![
            task_spec("t1", Some("investigate the failing gate")),
            task_spec("t2", None),
        ],
    };
    validate_plan(&spec, |_| true).expect("a two-task plan with no cycles must validate")
}

/// Opens a fresh store and registry, creates a two-instruction plan (`two_task_plan_with_instructions`)
/// owned by a freshly opened alive window, opens a second alive window and binds it to
/// `t1`, and returns `(store, registry, owner_window_id, reporter_window_id, plan_id)`.
/// `t2` is left deliberately unbound. Mirrors
/// `report_ledger.rs::store_registry_owner_and_bound_reporter`.
async fn store_registry_owner_and_bound_reporter(
    tag: &str,
) -> (PlanStore, WindowRegistry, String, String, String) {
    let mut store = PlanStore::open(&db_path(tag)).expect("PlanStore::open must succeed");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let owner = open_window(&registry).await;
    let create_permit = plan_permit::issue_create(&RequestOrigin::Window(owner.clone()), &registry)
        .expect("a freshly opened window must be able to create a plan");
    let plan_id = store
        .create_plan(&create_permit, &two_task_plan_with_instructions(), None)
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

// --- Writing an instruction-notice state through a real NoticePermit lands
// exactly against the targeted task, leaving an unrelated (but also instruction-bearing
// and bound) task's notice untouched. ---

#[tokio::test]
async fn writing_an_instruction_notice_state_updates_only_the_targeted_tasks_columns() {
    let (mut store, registry, owner, reporter, plan_id) =
        store_registry_owner_and_bound_reporter("notice-ledger-write").await;
    let table = LockTable::new();

    // Bind t2 as well, so its untouched Pending notice is a meaningful control.
    let bind_t2_guard = table.lock(&plan_id).await;
    let bind_t2_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner),
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

    let guard = table.lock(&plan_id).await;
    let permit = plan_permit::issue_notice(&reporter, "t1", guard, &store)
        .expect("the window currently bound to t1 must be authorized to record a notice");
    store
        .write_instruction_notice(&permit, InstructionNoticeState::Delivered)
        .expect("write_instruction_notice must succeed for a validly issued permit");

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

    let notice = t1
        .instruction_notice
        .as_ref()
        .expect("t1 must carry a notice after the write");
    assert_eq!(notice.state, InstructionNoticeState::Delivered);
    assert!(
        notice.at.is_some(),
        "delivered must be stamped with a timestamp, got {:?}",
        notice.at
    );

    let t2_notice = t2
        .instruction_notice
        .as_ref()
        .expect("t2 must still carry the pending notice binding gave it");
    assert_eq!(
        t2_notice.state,
        InstructionNoticeState::Pending,
        "writing t1's notice must not disturb t2's"
    );
    assert_eq!(t2_notice.at, None);
}

// --- Binding a task's window sets its instruction notice to pending only when
// the task carries an instruction; a task with none gets no notice at all. ---

#[tokio::test]
async fn binding_a_task_sets_its_instruction_notice_to_pending_only_when_it_carries_one() {
    let mut store =
        PlanStore::open(&db_path("notice-ledger-bind-pending")).expect("PlanStore::open");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let owner = open_window(&registry).await;
    let create_permit = plan_permit::issue_create(&RequestOrigin::Window(owner.clone()), &registry)
        .expect("the owner must be able to create a plan");
    let plan_id = store
        .create_plan(
            &create_permit,
            &task_plan_one_with_and_one_without_instruction(),
            None,
        )
        .expect("create_plan must succeed");
    let table = LockTable::new();

    let t1_window = open_window(&registry).await;
    let bind_t1_guard = table.lock(&plan_id).await;
    let bind_t1_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner.clone()),
        ChangeKind::OpenTaskWindow,
        bind_t1_guard,
        &store,
        &registry,
    )
    .expect("the owner must be able to bind t1's window");
    store
        .set_task_window(&bind_t1_permit, "t1", &t1_window)
        .expect("set_task_window must succeed for t1");
    drop(bind_t1_permit);

    let t2_window = open_window(&registry).await;
    let bind_t2_guard = table.lock(&plan_id).await;
    let bind_t2_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner),
        ChangeKind::OpenTaskWindow,
        bind_t2_guard,
        &store,
        &registry,
    )
    .expect("the owner must be able to bind t2's window");
    store
        .set_task_window(&bind_t2_permit, "t2", &t2_window)
        .expect("set_task_window must succeed for t2");
    drop(bind_t2_permit);

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

    let t1_notice = t1
        .instruction_notice
        .as_ref()
        .expect("t1 carries an instruction, so binding it must create a pending notice");
    assert_eq!(t1_notice.state, InstructionNoticeState::Pending);
    assert_eq!(t1_notice.at, None);
    assert_eq!(
        t2.instruction_notice, None,
        "t2 carries no instruction, so binding it must not create a notice"
    );
}

// --- Rebinding a task's window resets its instruction notice to pending, even
// after it reached delivered. ---

#[tokio::test]
async fn rebinding_a_task_with_an_instruction_resets_its_notice_to_pending() {
    let (mut store, registry, owner, reporter, plan_id) =
        store_registry_owner_and_bound_reporter("notice-ledger-rebind-resets").await;
    let table = LockTable::new();

    let deliver_guard = table.lock(&plan_id).await;
    let deliver_permit = plan_permit::issue_notice(&reporter, "t1", deliver_guard, &store)
        .expect("t1's bound window must be authorized to record a notice");
    store
        .write_instruction_notice(&deliver_permit, InstructionNoticeState::Delivered)
        .expect("write_instruction_notice must succeed");
    drop(deliver_permit);

    let rebind_guard = table.lock(&plan_id).await;
    let rebind_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner),
        ChangeKind::OpenTaskWindow,
        rebind_guard,
        &store,
        &registry,
    )
    .expect("the owner must be able to rebind t1's window");
    let new_window = open_window(&registry).await;
    store
        .set_task_window(&rebind_permit, "t1", &new_window)
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
    let notice = t1
        .instruction_notice
        .as_ref()
        .expect("t1 still carries its instruction, so a rebind must leave it a pending notice");
    assert_eq!(
        notice.state,
        InstructionNoticeState::Pending,
        "a rebind is a new attempt at delivering the same instruction, so a delivered notice \
         must reset to pending rather than survive the old binding"
    );
    assert_eq!(notice.at, None);
}

// --- replace_tasks carries a surviving task_id's instruction-notice state
// across when the replacement still carries an instruction; a brand-new task_id starts
// with no notice even though it carries one, since it has never been bound. ---

#[tokio::test]
async fn replacing_tasks_carries_over_a_survivors_instruction_notice_state_when_the_instruction_remains()
 {
    let (mut store, registry, owner, reporter, plan_id) =
        store_registry_owner_and_bound_reporter("notice-ledger-replace-carries").await;
    let table = LockTable::new();

    let deliver_guard = table.lock(&plan_id).await;
    let deliver_permit = plan_permit::issue_notice(&reporter, "t1", deliver_guard, &store)
        .expect("t1's bound window must be authorized to record a notice");
    store
        .write_instruction_notice(&deliver_permit, InstructionNoticeState::Delivered)
        .expect("write_instruction_notice must succeed");
    drop(deliver_permit);
    let before = store
        .get_plan(&plan_id)
        .expect("get_plan must not error")
        .expect("the plan must still exist");
    let at_before = before
        .tasks
        .iter()
        .find(|task| task.task_id == "t1")
        .and_then(|task| task.instruction_notice.as_ref())
        .and_then(|notice| notice.at)
        .expect("t1 must carry a delivered notice with a timestamp before the replace");

    let replacement_spec = PlanSpec {
        title: "instruction notice ledger replacement".to_string(),
        tasks: vec![
            task_spec("t1", Some("investigate the failing gate, retitled")),
            task_spec("t3", Some("a brand new task with its own instruction")),
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
    let t1 = detail
        .tasks
        .iter()
        .find(|task| task.task_id == "t1")
        .expect("t1 must survive the replace");
    let notice = t1
        .instruction_notice
        .as_ref()
        .expect("t1's notice must survive a replace that keeps its instruction");
    assert_eq!(notice.state, InstructionNoticeState::Delivered);
    assert_eq!(
        notice.at,
        Some(at_before),
        "the carried-over notice must keep its original timestamp, not be re-stamped"
    );

    let t3 = detail
        .tasks
        .iter()
        .find(|task| task.task_id == "t3")
        .expect("t3 must be present as a brand-new task");
    assert_eq!(
        t3.instruction_notice, None,
        "a brand-new task_id has never been bound to a window, so it must start with no \
         notice even though it carries an instruction"
    );
}

// --- replace_tasks clears a surviving task_id's instruction-notice state when
// the replacement's instruction is removed, even though its window binding still carries
// over. ---

#[tokio::test]
async fn replacing_tasks_clears_a_survivors_instruction_notice_state_when_the_instruction_is_removed()
 {
    let (mut store, registry, owner, reporter, plan_id) =
        store_registry_owner_and_bound_reporter("notice-ledger-replace-clears").await;
    let table = LockTable::new();

    let deliver_guard = table.lock(&plan_id).await;
    let deliver_permit = plan_permit::issue_notice(&reporter, "t1", deliver_guard, &store)
        .expect("t1's bound window must be authorized to record a notice");
    store
        .write_instruction_notice(&deliver_permit, InstructionNoticeState::Delivered)
        .expect("write_instruction_notice must succeed");
    drop(deliver_permit);

    let replacement_spec = PlanSpec {
        title: "instruction notice ledger replacement, instruction removed".to_string(),
        tasks: vec![
            task_spec("t1", None),
            task_spec("t2", Some("summarize the change")),
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
    let t1 = detail
        .tasks
        .iter()
        .find(|task| task.task_id == "t1")
        .expect("t1 must survive the replace");
    assert_eq!(
        t1.instruction_notice, None,
        "removing t1's instruction in the replacement must clear its notice"
    );
    assert_eq!(
        t1.window_id,
        Some(reporter),
        "the notice clearing must be specific to the removed instruction, not a side effect \
         of losing the window binding -- the binding itself is still a survivor and must \
         carry over"
    );
}

// --- Opening as a new boot turns a task's pending instruction notice into
// window_closed; a task with no instruction still carries none. ---

#[tokio::test]
async fn opening_as_a_new_boot_turns_a_pending_instruction_notice_into_window_closed() {
    let path = db_path("notice-ledger-new-boot");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let owner = open_window(&registry).await;
    let reporter = open_window(&registry).await;

    let plan_id = {
        let mut store = PlanStore::open(&path).expect("PlanStore::open must succeed");
        let create_permit =
            plan_permit::issue_create(&RequestOrigin::Window(owner.clone()), &registry)
                .expect("the owner must be able to create a plan");
        let plan_id = store
            .create_plan(
                &create_permit,
                &task_plan_one_with_and_one_without_instruction(),
                None,
            )
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
        plan_id
    };

    let store = PlanStore::open_as_new_boot(&path).expect("open_as_new_boot must succeed");
    let detail = store
        .get_plan(&plan_id)
        .expect("get_plan must not error")
        .expect("the plan must survive a new boot");
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
        t1.window_id, None,
        "a new boot must still clear the task's window binding"
    );
    let notice = t1
        .instruction_notice
        .as_ref()
        .expect("t1's pending notice must survive a new boot, turned into window_closed");
    assert_eq!(notice.state, InstructionNoticeState::WindowClosed);
    assert!(
        notice.at.is_some(),
        "window_closed must be stamped with a timestamp, got {:?}",
        notice.at
    );
    assert_eq!(
        t2.instruction_notice, None,
        "a task with no instruction must still carry no notice after a new boot"
    );
}

// --- A notice permit is granted only for the task_id a window is
// currently bound to, read fresh at issuance -- never from a caller-supplied fact carried
// over from before a rebind. Refused for: a task never bound to any window, an unrelated
// window, a task_id the plan does not have, a window whose binding was since replaced, and
// a plan_id this store has never created. The stale attempt's refusal must also leave the
// task's persisted state exactly what the new binding produced, never the old window's
// intended write: an old window cannot overwrite. ---

#[tokio::test]
async fn notice_permit_authorization_requires_a_current_task_binding_to_the_window() {
    let (mut store, registry, owner, reporter, plan_id) =
        store_registry_owner_and_bound_reporter("notice-ledger-authz-binding").await;
    let table = LockTable::new();

    let ok_guard = table.lock(&plan_id).await;
    assert!(
        plan_permit::issue_notice(&reporter, "t1", ok_guard, &store).is_ok(),
        "the window currently bound to t1 must be authorized to record a notice against it"
    );

    let t2_unbound_guard = table.lock(&plan_id).await;
    let t2_unbound_result = plan_permit::issue_notice(&reporter, "t2", t2_unbound_guard, &store);
    assert!(
        matches!(t2_unbound_result, Err(PermitError::InvalidState)),
        "a task with no window binding at all must be refused invalid_state, got \
         {t2_unbound_result:?}"
    );

    let unrelated = open_window(&registry).await;
    let unrelated_guard = table.lock(&plan_id).await;
    let unrelated_result = plan_permit::issue_notice(&unrelated, "t1", unrelated_guard, &store);
    assert!(
        matches!(unrelated_result, Err(PermitError::InvalidState)),
        "a window not bound to t1 must be refused invalid_state, got {unrelated_result:?}"
    );

    let missing_guard = table.lock(&plan_id).await;
    let missing_result =
        plan_permit::issue_notice(&reporter, "does-not-exist", missing_guard, &store);
    assert!(
        matches!(missing_result, Err(PermitError::InvalidState)),
        "a task_id this plan does not have must be refused invalid_state, got {missing_result:?}"
    );

    let rebind_target = open_window(&registry).await;
    let rebind_guard = table.lock(&plan_id).await;
    let rebind_permit = plan_permit::issue_change(
        &RequestOrigin::Window(owner),
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
    let stale_result = plan_permit::issue_notice(&reporter, "t1", stale_guard, &store);
    assert!(
        matches!(stale_result, Err(PermitError::InvalidState)),
        "a window whose task binding was replaced must be refused invalid_state for its old \
         task_id, got {stale_result:?}"
    );

    let detail = store
        .get_plan(&plan_id)
        .expect("get_plan must not error")
        .expect("the plan must still exist");
    let t1 = detail
        .tasks
        .iter()
        .find(|task| task.task_id == "t1")
        .expect("t1 must still be present");
    let notice = t1
        .instruction_notice
        .as_ref()
        .expect("the rebind must have left t1 with a pending notice");
    assert_eq!(
        notice.state,
        InstructionNoticeState::Pending,
        "the old window's refused attempt must not have changed t1's state: it must still be \
         exactly what the rebind produced"
    );
    assert_eq!(notice.at, None);

    let new_binding_guard = table.lock(&plan_id).await;
    assert!(
        plan_permit::issue_notice(&rebind_target, "t1", new_binding_guard, &store).is_ok(),
        "the newly bound window must be authorized to record a notice against t1"
    );

    let nonexistent_guard = table.lock("notice-ledger-plan-does-not-exist").await;
    let nonexistent_window = open_window(&registry).await;
    let nonexistent_result =
        plan_permit::issue_notice(&nonexistent_window, "t1", nonexistent_guard, &store);
    assert!(
        matches!(nonexistent_result, Err(PermitError::PlanNotFound)),
        "a plan_id this store has never created must be refused plan_not_found, got \
         {nonexistent_result:?}"
    );
}

// --- A NoticePermit moved into a spawn_blocking closure still licenses the
// write it was issued for (the door StoreGate::with_store -- src/ipc/chat.rs -- requires
// F: Send + 'static, the same shape production code must compile ReportPermit and
// ChangePermit into, mirrored here). ---

#[tokio::test]
async fn a_notice_permit_moved_into_spawn_blocking_still_licenses_a_store_write() {
    let (store, _registry, _owner, reporter, plan_id) =
        store_registry_owner_and_bound_reporter("notice-ledger-spawn-blocking").await;
    let table = LockTable::new();

    let guard = table.lock(&plan_id).await;
    let permit = plan_permit::issue_notice(&reporter, "t1", guard, &store)
        .expect("the bound window must be authorized to record a notice");

    let store = std::sync::Arc::new(std::sync::Mutex::new(store));
    let store_in_closure = store.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let mut guard = store_in_closure
            .lock()
            .expect("the store mutex must not be poisoned");
        guard.write_instruction_notice(&permit, InstructionNoticeState::Delivered)
    })
    .await
    .expect("the blocking task must not panic or be cancelled");

    assert!(
        outcome.is_ok(),
        "a NoticePermit moved into a spawn_blocking closure must still license the write it \
         was issued for, got {outcome:?}"
    );

    let store = store.lock().expect("the store mutex must not be poisoned");
    let detail = store
        .get_plan(&plan_id)
        .expect("get_plan must not error")
        .expect("the plan must still exist");
    let notice = detail
        .tasks
        .iter()
        .find(|task| task.task_id == "t1")
        .and_then(|task| task.instruction_notice.clone())
        .expect("the write issued through the moved permit must actually have landed");
    assert_eq!(notice.state, InstructionNoticeState::Delivered);
}
