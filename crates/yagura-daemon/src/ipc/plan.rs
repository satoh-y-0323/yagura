//! The plan ledger's IPC surface: [`PlanRuntime`] (store lifecycle, per-plan
//! lock table) and the `create_plan` / `update_plan` / `get_plan` /
//! `list_plans` / `delete_plan` handlers, plus the approver-only assignment,
//! owner and role-table handlers.
//!
//! Authorization is never derived here: every handler passes the connection's
//! [`RequestOrigin`] to [`plan_permit`]'s `issue_*` functions and writes only
//! through the permit they return. A structural change (`update_plan` /
//! `delete_plan`) first confirms the plan still exists, then takes that
//! plan's lock, then re-derives the grant from inside it --
//! `issue_change` decides relation and liveness fresh every time, so a grant
//! from a moment ago is never replayed as though it still held.

use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::Semaphore;

use yagura_protocol::{
    DaemonMessage, PlanInfo, PlanTaskInfo, PlanTaskSpec, RoleEntry, TaskAssignment,
    validate_launch_option_value,
};

use crate::approval::ApprovalBroker;
use crate::config::DaemonConfig;
use crate::control::RequestOrigin;
use crate::handoff::compose_plan_approved_notice;
use crate::inbox::Parcel;
use crate::live_policy::PolicyCell;
use crate::plan;
use crate::plan_permit::{self, ChangeKind, LockTable, PermitError};
use crate::plan_store::{
    ApproveOutcome, ClearOutcome, PlanDetail, PlanStore, PlanStoreError, PlanTaskInstructionNotice,
    PlanTaskRecord, PlanTaskReport,
};
use crate::profile;
use crate::roles::{RoleRow, RolesError, RolesTable, read_roles, save_roles as save_roles_file};
use crate::store::StoreError;
use crate::window::{WindowLiveness, WindowRegistry};

use super::store_gate::{StoreAccessError, StoreGate};
use super::{Outbound, error_msg, window_not_found_msg};

/// [`super::chat::ChatRuntime`]'s counterpart for the plan ledger: the store
/// gate and the per-plan lock table, together.
pub struct PlanRuntime {
    store: StoreGate<PlanStore>,
    locks: Arc<LockTable>,
}

impl PlanRuntime {
    /// A disabled runtime: every plan tool answers `unsupported`.
    /// [`super::IpcServer::new`] uses this so its own construction never
    /// touches a database.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            store: StoreGate::disabled(),
            locks: Arc::new(LockTable::new()),
        }
    }

    /// [`Self::disabled`], logging why (once) first.
    fn disabled_with_reason(reason: &str) -> Self {
        tracing::warn!("plan ledger disabled: {reason}");
        Self::disabled()
    }

    /// Opens the plan store as a new boot (clearing every plan's owner and
    /// task window binding, since no window id survives a restart), or
    /// returns a disabled runtime when no store path is resolved or the open
    /// fails.
    ///
    /// A caller in an async context must run this on a blocking thread
    /// (`tokio::task::spawn_blocking`): opening a database is blocking I/O.
    #[must_use]
    pub fn from_config(config: &DaemonConfig) -> Self {
        let Some(db_path) = config.store_db_path_resolved.as_ref() else {
            return Self::disabled_with_reason("no store database path is resolved");
        };
        match PlanStore::open_as_new_boot(db_path) {
            Ok(store) => {
                tracing::info!("plan store: {}", db_path.display());
                Self {
                    store: StoreGate::opened(store),
                    locks: Arc::new(LockTable::new()),
                }
            }
            Err(err) => {
                Self::disabled_with_reason(&format!("failed to open the plan store: {err}"))
            }
        }
    }

    /// The lock table this runtime's own handlers acquire a plan's guard
    /// from -- the same table a caller holding a guard across a request (a
    /// test, typically) inspects through [`LockTable::len`].
    #[must_use]
    pub fn locks(&self) -> Arc<LockTable> {
        Arc::clone(&self.locks)
    }

    pub(super) fn is_enabled(&self) -> bool {
        self.store.is_enabled()
    }

    pub(super) async fn with_store<R, F>(&self, f: F) -> Result<R, StoreAccessError>
    where
        F: FnOnce(&mut PlanStore) -> R + Send + 'static,
        R: Send + 'static,
    {
        self.store.with_store(f).await
    }
}

/// Reads the role table and the profile listing together, on a blocking
/// thread, admitted only by `permits`: both are blocking
/// file-system reads that a request-path handler must never perform on its
/// own async task, the same discipline `workspace_walks`
/// (`window.rs`'s `open_impl`) holds for `resolve_key_blocking`.
// The one caller of both blocking reads, admitted by `permits`.
#[allow(clippy::disallowed_methods)]
pub(super) async fn load_role_and_profile_state_blocking(
    config: &DaemonConfig,
    permits: &Semaphore,
) -> (Result<RolesTable, RolesError>, profile::ProfileListing) {
    let fallback = || {
        (
            Ok(RolesTable {
                rows: Vec::new(),
                default_role: None,
            }),
            profile::ProfileListing {
                profiles: Vec::new(),
                warnings: Vec::new(),
            },
        )
    };
    // The semaphore is never closed in this daemon's lifetime; a closed
    // acquire (which cannot happen in practice) degrades to "read nothing"
    // rather than hanging a request-path handler forever.
    let Ok(_permit) = permits.acquire().await else {
        return fallback();
    };
    let config = config.clone();
    tokio::task::spawn_blocking(move || {
        let roles = match config.roles_path.as_deref() {
            Some(path) => read_roles(path),
            None => Ok(RolesTable {
                rows: Vec::new(),
                default_role: None,
            }),
        };
        let profiles = profile::list_profiles(&config);
        (roles, profiles)
    })
    .await
    .unwrap_or_else(|_join_err| fallback())
}

/// [`plan::validate_plan`] on a blocking thread admitted by `permits`: its
/// work grows with the plan's size, and on the async runtime it would hold up
/// every other request for as long as it runs.
///
/// Returns `None` when the check could not run (the runtime is shutting down).
async fn validate_plan_blocking(
    spec: plan::PlanSpec,
    profile_ids: HashSet<String>,
    permits: &Semaphore,
) -> Option<Result<plan::ValidatedPlan, Vec<plan::PlanError>>> {
    let _permit = permits.acquire().await.ok()?;
    tokio::task::spawn_blocking(move || plan::validate_plan(&spec, |id| profile_ids.contains(id)))
        .await
        .ok()
}

// --- create_plan ---------------------------------------------------------

/// Why `create_plan`'s write failed, beyond a permit refusal.
enum CreateOutcome {
    Create(PlanStoreError),
    ReadBack(StoreError),
    /// The row `create_plan` just inserted was gone by the time this same
    /// call read it back. Unreachable in practice: the store's own mutex is
    /// held for the whole closure, so nothing else could have deleted it in
    /// between.
    Vanished,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_create_plan(
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    config: &DaemonConfig,
    policy: &PolicyCell,
    permits: &Semaphore,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    title: String,
    tasks: Vec<PlanTaskSpec>,
) {
    if !ledger.is_enabled() {
        outbound.reply(&unsupported_msg());
        return;
    }

    let create_permit = match plan_permit::issue_create(&state.origin, registry) {
        Ok(permit) => permit,
        Err(err) => {
            outbound.reply(&permit_error_msg(err));
            return;
        }
    };

    let (roles_result, profile_listing) =
        load_role_and_profile_state_blocking(config, permits).await;
    let profile_ids: HashSet<String> = profile_listing
        .profiles
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    let spec = plan::PlanSpec {
        title,
        tasks: tasks.into_iter().map(to_task_spec).collect(),
    };
    let validated = match validate_plan_blocking(spec, profile_ids, permits).await {
        Some(Ok(validated)) => validated,
        Some(Err(errors)) => {
            outbound.reply(&invalid_argument_msg(&errors));
            return;
        }
        None => {
            outbound.reply(&internal_msg());
            return;
        }
    };

    // A plan created while the operator must confirm plans starts waiting
    // for that confirmation; `plan_permit::issue_change` reads this same
    // column back to gate `open_task_window` on it.
    let review = policy
        .snapshot()
        .plans_require_approval
        .then_some(crate::plan_store::PlanReview::Pending);
    let result = ledger
        .with_store(move |store| -> Result<PlanDetail, CreateOutcome> {
            let plan_id = store
                .create_plan(&create_permit, &validated, review)
                .map_err(CreateOutcome::Create)?;
            store
                .get_plan(&plan_id)
                .map_err(CreateOutcome::ReadBack)?
                .ok_or(CreateOutcome::Vanished)
        })
        .await;

    match result {
        Ok(Ok(detail)) => {
            let (roles, roles_warning) = split_roles_result(roles_result);
            let info = build_plan_info_from_detail(detail, roles.as_ref(), roles_warning);
            broker.notify_approvers(&DaemonMessage::PlanChanged {
                plan_id: info.plan_id.clone(),
            });
            outbound.reply(&DaemonMessage::PlanDetail { plan: info });
        }
        Ok(Err(CreateOutcome::Create(PlanStoreError::ResourceExhausted(_)))) => {
            outbound.reply(&resource_exhausted_msg());
        }
        Ok(Err(CreateOutcome::Create(PlanStoreError::Store(store_err)))) => {
            tracing::warn!(error = %store_err, "plan create failed");
            outbound.reply(&internal_msg());
        }
        Ok(Err(CreateOutcome::ReadBack(store_err))) => {
            tracing::warn!(error = %store_err, "plan read-back after create failed");
            outbound.reply(&internal_msg());
        }
        Ok(Err(CreateOutcome::Vanished)) => {
            tracing::warn!("plan create succeeded but the row could not be read back");
            outbound.reply(&internal_msg());
        }
        Err(access_err) => outbound.reply(&map_access_error(access_err)),
    }
}

// --- update_plan -----------------------------------------------------------

/// Why `update_plan`'s write failed, beyond a permit refusal.
enum UpdateOutcome {
    Permit(PermitError),
    Write(StoreError),
    /// Unreachable in practice, the same way [`CreateOutcome::Vanished`] is.
    Vanished,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_update_plan(
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    config: &DaemonConfig,
    permits: &Semaphore,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    plan_id: String,
    title: Option<String>,
    tasks: Vec<PlanTaskSpec>,
) {
    if !ledger.is_enabled() {
        outbound.reply(&unsupported_msg());
        return;
    }
    if matches!(state.origin, RequestOrigin::External) {
        outbound.reply(&auth_failed_msg());
        return;
    }

    let (roles_result, profile_listing) =
        load_role_and_profile_state_blocking(config, permits).await;

    // Doubles as the existence check every change request needs before a
    // lock is ever taken: a `plan_id` nobody ever created must
    // never grow the lock table.
    let existing_title = {
        let plan_id = plan_id.clone();
        match ledger
            .with_store(move |store| store.get_plan(&plan_id))
            .await
        {
            Ok(Ok(Some(detail))) => detail.title,
            Ok(Ok(None)) => {
                outbound.reply(&plan_not_found_msg());
                return;
            }
            Ok(Err(store_err)) => {
                tracing::warn!(error = %store_err, "plan read-before-update failed");
                outbound.reply(&internal_msg());
                return;
            }
            Err(access_err) => {
                outbound.reply(&map_access_error(access_err));
                return;
            }
        }
    };

    let profile_ids: HashSet<String> = profile_listing
        .profiles
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    let spec = plan::PlanSpec {
        title: title.unwrap_or(existing_title),
        tasks: tasks.into_iter().map(to_task_spec).collect(),
    };
    let validated = match validate_plan_blocking(spec, profile_ids, permits).await {
        Some(Ok(validated)) => validated,
        Some(Err(errors)) => {
            outbound.reply(&invalid_argument_msg(&errors));
            return;
        }
        None => {
            outbound.reply(&internal_msg());
            return;
        }
    };

    let guard = ledger.locks().lock(&plan_id).await;
    let origin = state.origin.clone();
    let registry = Arc::clone(registry);
    let result = ledger
        .with_store(move |store| -> Result<PlanDetail, UpdateOutcome> {
            let permit =
                plan_permit::issue_change(&origin, ChangeKind::UpdatePlan, guard, store, &registry)
                    .map_err(UpdateOutcome::Permit)?;
            store
                .replace_tasks(&permit, &validated)
                .map_err(UpdateOutcome::Write)?;
            store
                .get_plan(permit.plan_id())
                .map_err(UpdateOutcome::Write)?
                .ok_or(UpdateOutcome::Vanished)
        })
        .await;

    match result {
        Ok(Ok(detail)) => {
            let (roles, roles_warning) = split_roles_result(roles_result);
            let info = build_plan_info_from_detail(detail, roles.as_ref(), roles_warning);
            broker.notify_approvers(&DaemonMessage::PlanChanged {
                plan_id: info.plan_id.clone(),
            });
            outbound.reply(&DaemonMessage::PlanDetail { plan: info });
        }
        Ok(Err(UpdateOutcome::Permit(PermitError::PlanNotFound))) => {
            // Discovered gone only once the lock was finally taken (a
            // concurrent delete raced this request): the entry a moment ago
            // named a real plan, so it is removed now rather than left
            // behind.
            ledger.locks().remove(&plan_id);
            outbound.reply(&plan_not_found_msg());
        }
        Ok(Err(UpdateOutcome::Permit(err))) => outbound.reply(&permit_error_msg(err)),
        Ok(Err(UpdateOutcome::Write(store_err))) => {
            tracing::warn!(error = %store_err, "plan update failed");
            outbound.reply(&internal_msg());
        }
        Ok(Err(UpdateOutcome::Vanished)) => {
            tracing::warn!("plan update succeeded but the row could not be read back");
            outbound.reply(&internal_msg());
        }
        Err(access_err) => outbound.reply(&map_access_error(access_err)),
    }
}

// --- get_plan / list_plans -------------------------------------------------

/// Why `get_plan`'s read failed, beyond a permit refusal.
enum ReadOutcome {
    Permit(PermitError),
    Store(StoreError),
    /// The permit was granted (an approver reaches every id unconditionally)
    /// but the id names nothing: the same refusal a window's own
    /// out-of-range read gets.
    NotFound,
}

pub(super) async fn handle_get_plan(
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    config: &DaemonConfig,
    permits: &Semaphore,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    plan_id: String,
) {
    if !ledger.is_enabled() {
        outbound.reply(&unsupported_msg());
        return;
    }
    if matches!(state.origin, RequestOrigin::External) {
        outbound.reply(&auth_failed_msg());
        return;
    }

    let (roles_result, _profiles) = load_role_and_profile_state_blocking(config, permits).await;

    let origin = state.origin.clone();
    let registry = Arc::clone(registry);
    let result = ledger
        .with_store(move |store| -> Result<PlanDetail, ReadOutcome> {
            let permit = plan_permit::issue_read(&origin, &plan_id, store, &registry)
                .map_err(ReadOutcome::Permit)?;
            store
                .get_plan(permit.plan_id())
                .map_err(ReadOutcome::Store)?
                .ok_or(ReadOutcome::NotFound)
        })
        .await;

    match result {
        Ok(Ok(detail)) => {
            let (roles, roles_warning) = split_roles_result(roles_result);
            let info = build_plan_info_from_detail(detail, roles.as_ref(), roles_warning);
            outbound.reply(&DaemonMessage::PlanDetail { plan: info });
        }
        Ok(Err(ReadOutcome::Permit(err))) => outbound.reply(&permit_error_msg(err)),
        Ok(Err(ReadOutcome::NotFound)) => outbound.reply(&plan_not_found_msg()),
        Ok(Err(ReadOutcome::Store(store_err))) => {
            tracing::warn!(error = %store_err, "plan read failed");
            outbound.reply(&internal_msg());
        }
        Err(access_err) => outbound.reply(&map_access_error(access_err)),
    }
}

pub(super) async fn handle_list_plans(
    ledger: &PlanRuntime,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
) {
    if !ledger.is_enabled() {
        outbound.reply(&unsupported_msg());
        return;
    }
    if matches!(state.origin, RequestOrigin::External) {
        outbound.reply(&auth_failed_msg());
        return;
    }
    let origin = state.origin.clone();
    let result = ledger
        .with_store(move |store| list_visible_plans(store, &origin))
        .await;
    match result {
        Ok(Ok(plans)) => outbound.reply(&DaemonMessage::PlanList { plans }),
        Ok(Err(store_err)) => {
            tracing::warn!(error = %store_err, "plan listing failed");
            outbound.reply(&internal_msg());
        }
        Err(access_err) => outbound.reply(&map_access_error(access_err)),
    }
}

/// The plans `origin` may see: every plan for an approver, and for a window
/// only those it owns or holds a task binding in.
fn list_visible_plans(
    store: &PlanStore,
    origin: &RequestOrigin,
) -> Result<Vec<yagura_protocol::PlanSummary>, StoreError> {
    let summaries = store.list_plans()?;
    let mut visible = Vec::with_capacity(summaries.len());
    for summary in summaries {
        let include = match origin {
            RequestOrigin::Approver => true,
            RequestOrigin::Window(window_id) => {
                summary.owner_window.as_deref() == Some(window_id.as_str())
                    || store.get_plan(&summary.plan_id)?.is_some_and(|detail| {
                        detail
                            .tasks
                            .iter()
                            .any(|task| task.window_id.as_deref() == Some(window_id.as_str()))
                    })
            }
            RequestOrigin::External => false,
        };
        if include {
            visible.push(yagura_protocol::PlanSummary {
                plan_id: summary.plan_id,
                title: summary.title,
                owner_window: summary.owner_window,
                task_count: u32::try_from(summary.task_count).unwrap_or(u32::MAX),
                review: summary.review,
                unseen_changes: summary.unseen_changes,
            });
        }
    }
    Ok(visible)
}

// --- delete_plan -----------------------------------------------------------

/// Why `delete_plan`'s write failed, beyond a permit refusal.
enum ChangeOutcome {
    Permit(PermitError),
    Write(StoreError),
}

/// Whether `plan_id` currently names a stored plan, or the reply to send in
/// its place when the check itself could not run.
// The error is the wire message itself, as in `validate_task_assignment`.
#[allow(clippy::result_large_err)]
pub(super) async fn plan_exists(
    ledger: &PlanRuntime,
    plan_id: &str,
) -> Result<bool, DaemonMessage> {
    let plan_id = plan_id.to_string();
    match ledger
        .with_store(move |store| store.get_plan(&plan_id).map(|detail| detail.is_some()))
        .await
    {
        Ok(Ok(exists)) => Ok(exists),
        Ok(Err(store_err)) => {
            tracing::warn!(error = %store_err, "plan existence check failed");
            Err(internal_msg())
        }
        Err(access_err) => Err(map_access_error(access_err)),
    }
}

/// Why a task notice state was not recorded under the plan's lock.
enum NoticeRefusal {
    Permit(PermitError),
    Write(StoreError),
}

/// Records `state` for `task_id`'s instruction notice while the task is still
/// bound to `window_id`, and tells the approvers the plan changed. A task
/// rebound elsewhere since is left to its new window.
pub(super) async fn record_task_notice(
    ledger: &PlanRuntime,
    broker: &ApprovalBroker,
    window_id: &str,
    plan_id: &str,
    task_id: &str,
    state: yagura_protocol::InstructionNoticeState,
) {
    // Checked before locking: locking an id creates its lock-table entry.
    if !matches!(plan_exists(ledger, plan_id).await, Ok(true)) {
        return;
    }
    let guard = ledger.locks().lock(plan_id).await;
    let window_id = window_id.to_string();
    let task_id = task_id.to_string();
    let recorded = ledger
        .with_store(move |store| {
            let permit = plan_permit::issue_notice(&window_id, &task_id, guard, store)
                .map_err(NoticeRefusal::Permit)?;
            store
                .write_instruction_notice(&permit, state)
                .map_err(NoticeRefusal::Write)
        })
        .await;
    match recorded {
        Ok(Ok(())) => {
            broker.notify_approvers(&DaemonMessage::PlanChanged {
                plan_id: plan_id.to_string(),
            });
        }
        Ok(Err(NoticeRefusal::Permit(PermitError::PlanNotFound))) => {
            // Deleted between the existence check and the lock.
            ledger.locks().remove(plan_id);
        }
        Ok(Err(NoticeRefusal::Permit(_))) => {}
        Ok(Err(NoticeRefusal::Write(err))) => {
            tracing::warn!(error = %err, "task notice state write failed");
        }
        Err(_) => {
            tracing::warn!("the plan store became unavailable while recording a task notice");
        }
    }
}

pub(super) async fn handle_delete_plan(
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    plan_id: String,
) {
    if !ledger.is_enabled() {
        outbound.reply(&unsupported_msg());
        return;
    }
    if matches!(state.origin, RequestOrigin::External) {
        outbound.reply(&auth_failed_msg());
        return;
    }

    match plan_exists(ledger, &plan_id).await {
        Ok(true) => {}
        Ok(false) => {
            outbound.reply(&plan_not_found_msg());
            return;
        }
        Err(msg) => {
            outbound.reply(&msg);
            return;
        }
    }

    let guard = ledger.locks().lock(&plan_id).await;
    let origin = state.origin.clone();
    let registry = Arc::clone(registry);
    let result = ledger
        .with_store(move |store| -> Result<(), ChangeOutcome> {
            let permit =
                plan_permit::issue_change(&origin, ChangeKind::DeletePlan, guard, store, &registry)
                    .map_err(ChangeOutcome::Permit)?;
            store.delete_plan(permit).map_err(ChangeOutcome::Write)
        })
        .await;

    match result {
        Ok(Ok(())) => {
            ledger.locks().remove(&plan_id);
            broker.notify_approvers(&DaemonMessage::PlanChanged {
                plan_id: plan_id.clone(),
            });
            outbound.reply(&DaemonMessage::PlanDeleted { plan_id });
        }
        Ok(Err(ChangeOutcome::Permit(PermitError::PlanNotFound))) => {
            ledger.locks().remove(&plan_id);
            outbound.reply(&plan_not_found_msg());
        }
        Ok(Err(ChangeOutcome::Permit(err))) => outbound.reply(&permit_error_msg(err)),
        Ok(Err(ChangeOutcome::Write(store_err))) => {
            tracing::warn!(error = %store_err, "plan delete failed");
            outbound.reply(&internal_msg());
        }
        Err(access_err) => outbound.reply(&map_access_error(access_err)),
    }
}

// --- user assignment override / owner nomination --------------------------

/// Turns a `ChangeKind`-guarded write's result into a reply plus the
/// `plan_changed` push every structural change makes, shared by
/// [`handle_set_task_assignment`], [`handle_clear_task_assignment`] and
/// [`handle_set_plan_owner`]. [`handle_update_plan`] and [`handle_delete_plan`]
/// answer this same shape themselves, since each also has its own
/// argument-validation or id-echoing step around it.
async fn reply_with_changed_plan(
    ledger: &PlanRuntime,
    broker: &Arc<ApprovalBroker>,
    outbound: &Arc<Outbound>,
    plan_id: &str,
    roles_result: Result<RolesTable, RolesError>,
    result: Result<Result<PlanDetail, UpdateOutcome>, StoreAccessError>,
) {
    match result {
        Ok(Ok(detail)) => {
            let (roles, roles_warning) = split_roles_result(roles_result);
            let info = build_plan_info_from_detail(detail, roles.as_ref(), roles_warning);
            broker.notify_approvers(&DaemonMessage::PlanChanged {
                plan_id: info.plan_id.clone(),
            });
            outbound.reply(&DaemonMessage::PlanDetail { plan: info });
        }
        Ok(Err(UpdateOutcome::Permit(PermitError::PlanNotFound))) => {
            // The same race `handle_update_plan` guards against: a plan_id
            // that named a real plan a moment ago is gone by the time the
            // lock was actually taken.
            ledger.locks().remove(plan_id);
            outbound.reply(&plan_not_found_msg());
        }
        Ok(Err(UpdateOutcome::Permit(err))) => outbound.reply(&permit_error_msg(err)),
        Ok(Err(UpdateOutcome::Write(store_err))) => {
            tracing::warn!(error = %store_err, "plan change failed");
            outbound.reply(&internal_msg());
        }
        Ok(Err(UpdateOutcome::Vanished)) => {
            tracing::warn!("plan change succeeded but the row could not be read back");
            outbound.reply(&internal_msg());
        }
        Err(access_err) => outbound.reply(&map_access_error(access_err)),
    }
}

/// Validates a lone [`TaskAssignment`] the way [`plan::validate_plan`]
/// validates a task's own `profile`/`model`/`effort` triple: `profile` must
/// be one `profile_exists` reports, `model`/`effort` must each pass
/// [`validate_launch_option_value`]. Never echoes the rejected value back.
// The error is the wire message itself, the same reason
// `IpcServer::plan_from_profile` carries the same allow.
#[allow(clippy::result_large_err)]
fn validate_task_assignment(
    assignment: &TaskAssignment,
    profile_exists: impl Fn(&str) -> bool,
) -> Result<plan::Assignment, DaemonMessage> {
    if !profile_exists(&assignment.profile) {
        return Err(error_msg("invalid_argument", "profile does not exist"));
    }
    if let Some(model) = &assignment.model
        && validate_launch_option_value(model).is_err()
    {
        return Err(error_msg("invalid_argument", "model value is invalid"));
    }
    if let Some(effort) = &assignment.effort
        && validate_launch_option_value(effort).is_err()
    {
        return Err(error_msg("invalid_argument", "effort value is invalid"));
    }
    Ok(plan::Assignment {
        profile: assignment.profile.clone(),
        model: assignment.model.clone(),
        effort: assignment.effort.clone(),
    })
}

/// Sets a task's user override, approver-only regardless of the plan's
/// state or existence (a write only the GUI may make never reveals to any
/// other caller whether a plan_id is real).
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_set_task_assignment(
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    config: &DaemonConfig,
    permits: &Semaphore,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    plan_id: String,
    task_id: String,
    assignment: TaskAssignment,
) {
    if !ledger.is_enabled() {
        outbound.reply(&unsupported_msg());
        return;
    }
    if !matches!(state.origin, RequestOrigin::Approver) {
        outbound.reply(&auth_failed_msg());
        return;
    }

    let (roles_result, profile_listing) =
        load_role_and_profile_state_blocking(config, permits).await;
    let profile_ids: HashSet<&str> = profile_listing
        .profiles
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    let validated = match validate_task_assignment(&assignment, |id| profile_ids.contains(id)) {
        Ok(assignment) => assignment,
        Err(message) => {
            outbound.reply(&message);
            return;
        }
    };

    match plan_exists(ledger, &plan_id).await {
        Ok(true) => {}
        Ok(false) => {
            outbound.reply(&plan_not_found_msg());
            return;
        }
        Err(message) => {
            outbound.reply(&message);
            return;
        }
    }

    let guard = ledger.locks().lock(&plan_id).await;
    let origin = state.origin.clone();
    let registry = Arc::clone(registry);
    let task_id_for_write = task_id.clone();
    let result = ledger
        .with_store(move |store| -> Result<PlanDetail, UpdateOutcome> {
            let permit = plan_permit::issue_change(
                &origin,
                ChangeKind::SetTaskAssignment,
                guard,
                store,
                &registry,
            )
            .map_err(UpdateOutcome::Permit)?;
            store
                .set_user_override(&permit, &task_id_for_write, Some(validated))
                .map_err(UpdateOutcome::Write)?;
            store
                .get_plan(permit.plan_id())
                .map_err(UpdateOutcome::Write)?
                .ok_or(UpdateOutcome::Vanished)
        })
        .await;

    reply_with_changed_plan(ledger, broker, outbound, &plan_id, roles_result, result).await;
}

/// Clears a task's user override. Same approver-only, existence-then-lock
/// order as [`handle_set_task_assignment`]; unlike setting one, there is
/// nothing to validate.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_clear_task_assignment(
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    config: &DaemonConfig,
    permits: &Semaphore,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    plan_id: String,
    task_id: String,
) {
    if !ledger.is_enabled() {
        outbound.reply(&unsupported_msg());
        return;
    }
    if !matches!(state.origin, RequestOrigin::Approver) {
        outbound.reply(&auth_failed_msg());
        return;
    }

    match plan_exists(ledger, &plan_id).await {
        Ok(true) => {}
        Ok(false) => {
            outbound.reply(&plan_not_found_msg());
            return;
        }
        Err(message) => {
            outbound.reply(&message);
            return;
        }
    }

    let (roles_result, _profiles) = load_role_and_profile_state_blocking(config, permits).await;

    let guard = ledger.locks().lock(&plan_id).await;
    let origin = state.origin.clone();
    let registry = Arc::clone(registry);
    let task_id_for_write = task_id.clone();
    let result = ledger
        .with_store(move |store| -> Result<PlanDetail, UpdateOutcome> {
            let permit = plan_permit::issue_change(
                &origin,
                ChangeKind::ClearTaskAssignment,
                guard,
                store,
                &registry,
            )
            .map_err(UpdateOutcome::Permit)?;
            store
                .set_user_override(&permit, &task_id_for_write, None)
                .map_err(UpdateOutcome::Write)?;
            store
                .get_plan(permit.plan_id())
                .map_err(UpdateOutcome::Write)?
                .ok_or(UpdateOutcome::Vanished)
        })
        .await;

    reply_with_changed_plan(ledger, broker, outbound, &plan_id, roles_result, result).await;
}

/// Names a new owner for a plan. Approver-only; the target's own liveness is
/// re-read from inside the plan's lock by [`plan_permit::issue_nominate`],
/// so a candidate closed while a concurrent nomination is in
/// flight is judged by what is true once the lock is actually granted.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_set_plan_owner(
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    config: &DaemonConfig,
    permits: &Semaphore,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    plan_id: String,
    window_id: String,
) {
    if !ledger.is_enabled() {
        outbound.reply(&unsupported_msg());
        return;
    }
    if !matches!(state.origin, RequestOrigin::Approver) {
        outbound.reply(&auth_failed_msg());
        return;
    }

    match plan_exists(ledger, &plan_id).await {
        Ok(true) => {}
        Ok(false) => {
            outbound.reply(&plan_not_found_msg());
            return;
        }
        Err(message) => {
            outbound.reply(&message);
            return;
        }
    }

    let (roles_result, _profiles) = load_role_and_profile_state_blocking(config, permits).await;

    let guard = ledger.locks().lock(&plan_id).await;
    let origin = state.origin.clone();
    let registry = Arc::clone(registry);
    let result = ledger
        .with_store(move |store| -> Result<PlanDetail, UpdateOutcome> {
            let permit = plan_permit::issue_nominate(&origin, guard, &window_id, &registry)
                .map_err(UpdateOutcome::Permit)?;
            let nominated_plan_id = permit.plan_id().to_string();
            store.set_owner(permit).map_err(UpdateOutcome::Write)?;
            store
                .get_plan(&nominated_plan_id)
                .map_err(UpdateOutcome::Write)?
                .ok_or(UpdateOutcome::Vanished)
        })
        .await;

    reply_with_changed_plan(ledger, broker, outbound, &plan_id, roles_result, result).await;
}

// --- approve_plan / mark_plan_seen ----------------------------------------

/// Why `approve_plan`'s write failed, beyond a permit refusal.
enum ApproveWriteOutcome {
    Permit(PermitError),
    Write(StoreError),
    /// Unreachable in practice, the same way [`CreateOutcome::Vanished`] is.
    Vanished,
}

/// What `approve_plan`'s locked write found, if it got that far.
struct ApproveResult {
    outcome: ApproveOutcome,
    detail: PlanDetail,
    /// The plan's alive owner, read inside the same lock the write was
    /// decided under, only when `outcome` is [`ApproveOutcome::Approved`]:
    /// `None` for every other outcome, since nothing is queued for them.
    notify_owner: Option<String>,
}

/// Settles a plan the operator has been asked to review, approver-only.
/// Origin is checked before the plan's existence, the same order
/// [`handle_set_task_assignment`] uses, so a caller who is not an approver
/// never learns whether `plan_id` names a real plan.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_approve_plan(
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    config: &DaemonConfig,
    permits: &Semaphore,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    plan_id: String,
    version: u64,
) {
    if !ledger.is_enabled() {
        outbound.reply(&unsupported_msg());
        return;
    }
    if !matches!(state.origin, RequestOrigin::Approver) {
        outbound.reply(&auth_failed_msg());
        return;
    }

    match plan_exists(ledger, &plan_id).await {
        Ok(true) => {}
        Ok(false) => {
            outbound.reply(&plan_not_found_msg());
            return;
        }
        Err(message) => {
            outbound.reply(&message);
            return;
        }
    }

    let (roles_result, _profiles) = load_role_and_profile_state_blocking(config, permits).await;

    let guard = ledger.locks().lock(&plan_id).await;
    let origin = state.origin.clone();
    let registry_for_store = Arc::clone(registry);
    let result = ledger
        .with_store(move |store| -> Result<ApproveResult, ApproveWriteOutcome> {
            let permit = plan_permit::issue_change(
                &origin,
                ChangeKind::ApprovePlan,
                guard,
                store,
                &registry_for_store,
            )
            .map_err(ApproveWriteOutcome::Permit)?;
            let outcome = store
                .approve_plan(&permit, version)
                .map_err(ApproveWriteOutcome::Write)?;
            let notify_owner = matches!(outcome, ApproveOutcome::Approved)
                .then(|| {
                    plan_permit::resolve_owner_parent(store, &registry_for_store, permit.plan_id())
                })
                .flatten();
            let detail = store
                .get_plan(permit.plan_id())
                .map_err(ApproveWriteOutcome::Write)?
                .ok_or(ApproveWriteOutcome::Vanished)?;
            Ok(ApproveResult {
                outcome,
                detail,
                notify_owner,
            })
        })
        .await;

    match result {
        Ok(Ok(ApproveResult {
            outcome: ApproveOutcome::PlanGone,
            ..
        })) => {
            ledger.locks().remove(&plan_id);
            outbound.reply(&plan_not_found_msg());
        }
        Ok(Ok(ApproveResult {
            outcome: ApproveOutcome::NotReviewable | ApproveOutcome::VersionMismatch,
            ..
        })) => {
            outbound.reply(&invalid_state_msg());
        }
        Ok(Ok(ApproveResult {
            outcome: ApproveOutcome::AlreadyApproved,
            detail,
            ..
        })) => {
            let (roles, roles_warning) = split_roles_result(roles_result);
            let info = build_plan_info_from_detail(detail, roles.as_ref(), roles_warning);
            outbound.reply(&DaemonMessage::PlanDetail { plan: info });
        }
        Ok(Ok(ApproveResult {
            outcome: ApproveOutcome::Approved,
            detail,
            notify_owner,
        })) => {
            let (roles, roles_warning) = split_roles_result(roles_result);
            let info = build_plan_info_from_detail(detail, roles.as_ref(), roles_warning);
            broker.notify_approvers(&DaemonMessage::PlanChanged {
                plan_id: info.plan_id.clone(),
            });
            if let Some(owner_window) = notify_owner
                && let WindowLiveness::Alive(entry) = registry.liveness(&owner_window)
                && entry.delivers()
            {
                let key = format!("plan:{}", info.plan_id);
                // One notice per approved plan, however often it is approved.
                if !entry.inbox().holds(&key) {
                    let notice = Parcel::new(key, compose_plan_approved_notice(&info.plan_id));
                    // A notice the owner's inbox has no room for is dropped:
                    // the owner still reads the plan through get_plan.
                    let _ = super::notice_queue::deposit(registry, &entry, notice, None, None);
                }
            }
            outbound.reply(&DaemonMessage::PlanDetail { plan: info });
        }
        Ok(Err(ApproveWriteOutcome::Permit(PermitError::PlanNotFound))) => {
            ledger.locks().remove(&plan_id);
            outbound.reply(&plan_not_found_msg());
        }
        Ok(Err(ApproveWriteOutcome::Permit(err))) => outbound.reply(&permit_error_msg(err)),
        Ok(Err(ApproveWriteOutcome::Write(store_err))) => {
            tracing::warn!(error = %store_err, "plan approve failed");
            outbound.reply(&internal_msg());
        }
        Ok(Err(ApproveWriteOutcome::Vanished)) => {
            tracing::warn!("plan approve succeeded but the row could not be read back");
            outbound.reply(&internal_msg());
        }
        Err(access_err) => outbound.reply(&map_access_error(access_err)),
    }
}

/// Why `mark_plan_seen`'s write failed, beyond a permit refusal.
enum MarkSeenOutcome {
    Permit(PermitError),
    Write(StoreError),
    /// Unreachable in practice, the same way [`CreateOutcome::Vanished`] is.
    Vanished,
}

/// Clears every task's review mark on an approved plan, approver-only, but
/// only while the plan's task list is still at `version`: a stale call
/// leaves every mark in place and answers with the plan as it stands now,
/// carrying whatever marks the operator has not seen yet.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_mark_plan_seen(
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    config: &DaemonConfig,
    permits: &Semaphore,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    plan_id: String,
    version: u64,
) {
    if !ledger.is_enabled() {
        outbound.reply(&unsupported_msg());
        return;
    }
    if !matches!(state.origin, RequestOrigin::Approver) {
        outbound.reply(&auth_failed_msg());
        return;
    }

    match plan_exists(ledger, &plan_id).await {
        Ok(true) => {}
        Ok(false) => {
            outbound.reply(&plan_not_found_msg());
            return;
        }
        Err(message) => {
            outbound.reply(&message);
            return;
        }
    }

    let (roles_result, _profiles) = load_role_and_profile_state_blocking(config, permits).await;

    let guard = ledger.locks().lock(&plan_id).await;
    let origin = state.origin.clone();
    let registry = Arc::clone(registry);
    let result = ledger
        .with_store(
            move |store| -> Result<(ClearOutcome, PlanDetail), MarkSeenOutcome> {
                let permit = plan_permit::issue_change(
                    &origin,
                    ChangeKind::MarkPlanSeen,
                    guard,
                    store,
                    &registry,
                )
                .map_err(MarkSeenOutcome::Permit)?;
                let outcome = store
                    .clear_review_marks(&permit, version)
                    .map_err(MarkSeenOutcome::Write)?;
                let detail = store
                    .get_plan(permit.plan_id())
                    .map_err(MarkSeenOutcome::Write)?
                    .ok_or(MarkSeenOutcome::Vanished)?;
                Ok((outcome, detail))
            },
        )
        .await;

    match result {
        Ok(Ok((ClearOutcome::PlanGone, _))) => {
            ledger.locks().remove(&plan_id);
            outbound.reply(&plan_not_found_msg());
        }
        Ok(Ok((ClearOutcome::VersionMismatch, detail))) => {
            let (roles, roles_warning) = split_roles_result(roles_result);
            let info = build_plan_info_from_detail(detail, roles.as_ref(), roles_warning);
            outbound.reply(&DaemonMessage::PlanDetail { plan: info });
        }
        Ok(Ok((ClearOutcome::Cleared, detail))) => {
            let (roles, roles_warning) = split_roles_result(roles_result);
            let info = build_plan_info_from_detail(detail, roles.as_ref(), roles_warning);
            broker.notify_approvers(&DaemonMessage::PlanChanged {
                plan_id: info.plan_id.clone(),
            });
            outbound.reply(&DaemonMessage::PlanDetail { plan: info });
        }
        Ok(Err(MarkSeenOutcome::Permit(PermitError::PlanNotFound))) => {
            ledger.locks().remove(&plan_id);
            outbound.reply(&plan_not_found_msg());
        }
        Ok(Err(MarkSeenOutcome::Permit(err))) => outbound.reply(&permit_error_msg(err)),
        Ok(Err(MarkSeenOutcome::Write(store_err))) => {
            tracing::warn!(error = %store_err, "plan mark-seen failed");
            outbound.reply(&internal_msg());
        }
        Ok(Err(MarkSeenOutcome::Vanished)) => {
            tracing::warn!("plan mark-seen succeeded but the row could not be read back");
            outbound.reply(&internal_msg());
        }
        Err(access_err) => outbound.reply(&map_access_error(access_err)),
    }
}

// --- role table: list/save ------------------------------------------------

fn from_wire_role(entry: &RoleEntry) -> RoleRow {
    RoleRow {
        name: entry.name.clone(),
        profile: entry.profile.clone(),
        model: entry.model.clone(),
        effort: entry.effort.clone(),
    }
}

fn to_wire_roles(table: &RolesTable) -> Vec<RoleEntry> {
    table
        .rows
        .iter()
        .map(|row| RoleEntry {
            name: row.name.clone(),
            profile: row.profile.clone(),
            model: row.model.clone(),
            effort: row.effort.clone(),
        })
        .collect()
}

/// Reads the role table, readable by any identified connection (a window
/// resolving its own tasks, an approver editing it) but not `External`. Does
/// not depend on the plan ledger being enabled -- `roles.toml` is a
/// separate store from the plan database, so a daemon with no plan store
/// still answers this from the filesystem.
pub(super) async fn handle_list_roles(
    config: &DaemonConfig,
    permits: &Semaphore,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
) {
    if matches!(state.origin, RequestOrigin::External) {
        outbound.reply(&auth_failed_msg());
        return;
    }
    let (roles_result, _profiles) = load_role_and_profile_state_blocking(config, permits).await;
    match roles_result {
        Ok(table) => outbound.reply(&DaemonMessage::RoleList {
            roles: to_wire_roles(&table),
            default_role: table.default_role,
        }),
        Err(err) => outbound.reply(&error_msg("invalid_state", &err.to_string())),
    }
}

/// Writes `table` to `path` on a blocking thread, admitted by `permits` the
/// same way [`load_role_and_profile_state_blocking`]'s own read is.
/// `profile_ids` is already-read state (from
/// [`load_role_and_profile_state_blocking`]) rather than a fresh read here:
/// `profile::list_profiles` is called only from that one function
/// (`disallowed-methods` in `clippy.toml`).
async fn save_roles_blocking(
    permits: &Semaphore,
    path: std::path::PathBuf,
    table: RolesTable,
    profile_ids: HashSet<String>,
) -> Result<(), RolesError> {
    let Ok(_permit) = permits.acquire().await else {
        return Err(RolesError::Io(std::io::Error::other(
            "role-table access is unavailable",
        )));
    };
    tokio::task::spawn_blocking(move || {
        save_roles_file(&path, &table, |id| profile_ids.contains(id))
    })
    .await
    .unwrap_or_else(|_join_err| {
        Err(RolesError::Io(std::io::Error::other(
            "role-table save task failed",
        )))
    })
}

/// Replaces the whole role table, approver-only. Validation runs before the
/// write, so a rejected table never touches `roles.toml`
/// ([`save_roles_file`]'s own contract).
pub(super) async fn handle_save_roles(
    config: &DaemonConfig,
    permits: &Semaphore,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    roles: Vec<RoleEntry>,
    default_role: Option<String>,
) {
    if !matches!(state.origin, RequestOrigin::Approver) {
        outbound.reply(&auth_failed_msg());
        return;
    }
    let Some(path) = config.roles_path.clone() else {
        outbound.reply(&unsupported_msg());
        return;
    };

    let (_roles, profile_listing) = load_role_and_profile_state_blocking(config, permits).await;
    let profile_ids: HashSet<String> = profile_listing
        .profiles
        .into_iter()
        .map(|entry| entry.id)
        .collect();

    let table = RolesTable {
        rows: roles.iter().map(from_wire_role).collect(),
        default_role: default_role.clone(),
    };
    match save_roles_blocking(permits, path, table, profile_ids).await {
        Ok(()) => outbound.reply(&DaemonMessage::RoleList {
            roles,
            default_role,
        }),
        Err(RolesError::Io(io_err)) => {
            tracing::warn!(error = %io_err, "role table save failed");
            outbound.reply(&internal_msg());
        }
        Err(err) => outbound.reply(&error_msg("invalid_argument", &err.to_string())),
    }
}

// --- shared conversions ------------------------------------------------

fn to_task_spec(spec: PlanTaskSpec) -> plan::TaskSpec {
    plan::TaskSpec {
        task_id: spec.task_id,
        title: spec.title,
        role: spec.role,
        profile: spec.profile,
        model: spec.model,
        effort: spec.effort,
        depends_on: spec.depends_on,
        writes: spec.writes,
        instruction: spec.instruction,
    }
}

/// A stored [`PlanTaskReport`] as the wire's [`yagura_protocol::TaskReport`].
/// `reported_at` degrades to `0` on the (unreachable in practice) case of a
/// negative stored epoch-seconds value, the same fallback
/// `list_visible_plans`' `task_count` conversion uses for an out-of-range
/// count.
fn wire_task_report(report: PlanTaskReport) -> yagura_protocol::TaskReport {
    yagura_protocol::TaskReport {
        outcome: report.outcome,
        summary: report.summary,
        path: report.path,
        reported_at: u64::try_from(report.reported_at).unwrap_or(0),
    }
}

/// A stored [`PlanTaskInstructionNotice`] as the wire's
/// [`yagura_protocol::InstructionNotice`], degrading a negative stored `at`
/// to `0` the same way [`wire_task_report`] does.
fn wire_instruction_notice(
    notice: PlanTaskInstructionNotice,
) -> yagura_protocol::InstructionNotice {
    yagura_protocol::InstructionNotice {
        state: notice.state,
        at: notice.at.map(|at| u64::try_from(at).unwrap_or(0)),
    }
}

fn wire_assignment(assignment: &plan::Assignment) -> TaskAssignment {
    TaskAssignment {
        profile: assignment.profile.clone(),
        model: assignment.model.clone(),
        effort: assignment.effort.clone(),
    }
}

fn wire_assignment_source(source: plan::AssignmentSource) -> yagura_protocol::AssignmentSource {
    match source {
        plan::AssignmentSource::User => yagura_protocol::AssignmentSource::User,
        plan::AssignmentSource::Parent => yagura_protocol::AssignmentSource::Parent,
        plan::AssignmentSource::Role => yagura_protocol::AssignmentSource::Role,
        plan::AssignmentSource::Default => yagura_protocol::AssignmentSource::Default,
        plan::AssignmentSource::Unassigned => yagura_protocol::AssignmentSource::Unassigned,
    }
}

pub(super) fn wire_resolved(
    resolved: plan::ResolvedAssignment,
) -> yagura_protocol::ResolvedAssignment {
    yagura_protocol::ResolvedAssignment {
        source: wire_assignment_source(resolved.source),
        assignment: resolved.assignment.as_ref().map(wire_assignment),
    }
}

/// A stored task's `depends_on` and writes are already normalized (they
/// passed [`plan::validate_plan`] on the way in), so rebuilding a
/// [`plan::TaskSpec`] from a read-back row and re-running
/// [`plan::compute_waves`] reproduces the same wave numbers
/// [`plan::validate_plan`] would have derived at write time -- without this
/// module re-deriving the dependency-chain judgment itself.
fn task_spec_from_record(record: &PlanTaskRecord) -> plan::TaskSpec {
    let (profile, model, effort) = match &record.parent {
        Some(assignment) => (
            Some(assignment.profile.clone()),
            assignment.model.clone(),
            assignment.effort.clone(),
        ),
        None => (None, None, None),
    };
    plan::TaskSpec {
        task_id: record.task_id.clone(),
        title: record.title.clone(),
        role: record.role.clone(),
        profile,
        model,
        effort,
        depends_on: record.depends_on.clone(),
        writes: record.writes.clone(),
        instruction: record.instruction.clone(),
    }
}

/// Groups `task_ids` by the wave number at the same index in
/// `wave_numbers`, tasks within a wave kept in `task_ids`' own order --
/// exactly the grouping [`plan::validate_plan`] itself produces, reshaped
/// from [`plan::compute_waves`]'s flat output rather than re-derived.
fn group_waves(task_ids: &[String], wave_numbers: &[u32]) -> Vec<Vec<String>> {
    let mut waves: Vec<Vec<String>> = Vec::new();
    for (id, &wave) in task_ids.iter().zip(wave_numbers) {
        let index = wave as usize;
        if waves.len() <= index {
            waves.resize_with(index + 1, Vec::new);
        }
        if let Some(bucket) = waves.get_mut(index) {
            bucket.push(id.clone());
        }
    }
    waves
}

/// Builds the wire [`PlanInfo`] for a stored plan, resolving each task's
/// assignment against `roles` (`None` when the table could not be read,
/// `roles_warning` carrying why).
fn build_plan_info_from_detail(
    detail: PlanDetail,
    roles: Option<&RolesTable>,
    roles_warning: Option<String>,
) -> PlanInfo {
    let specs: Vec<plan::TaskSpec> = detail.tasks.iter().map(task_spec_from_record).collect();
    let wave_numbers = plan::compute_waves(&specs);
    let task_ids: Vec<String> = detail
        .tasks
        .iter()
        .map(|task| task.task_id.clone())
        .collect();
    let waves = group_waves(&task_ids, &wave_numbers);

    let tasks = detail
        .tasks
        .into_iter()
        .zip(wave_numbers)
        .map(|(record, wave)| {
            let PlanTaskRecord {
                task_id,
                title,
                role,
                depends_on,
                writes,
                parent,
                user,
                window_id,
                report,
                instruction,
                instruction_notice,
                review_mark,
                ..
            } = record;
            let (role_assignment, default_assignment) = roles
                .map(|table| table.role_and_default_for(role.as_deref()))
                .unwrap_or_default();
            let resolved = plan::resolve_assignment(
                user.as_ref(),
                parent.as_ref(),
                role_assignment.as_ref(),
                default_assignment.as_ref(),
            );
            PlanTaskInfo {
                task_id,
                title,
                role,
                depends_on,
                writes,
                wave,
                parent: parent.as_ref().map(wire_assignment),
                user: user.as_ref().map(wire_assignment),
                resolved: wire_resolved(resolved),
                window_id,
                report: report.map(wire_task_report),
                instruction,
                instruction_notice: instruction_notice.map(wire_instruction_notice),
                review_mark,
            }
        })
        .collect();

    PlanInfo {
        plan_id: detail.plan_id,
        title: detail.title,
        cwd: detail.cwd,
        owner_window: detail.owner_window,
        waves,
        tasks,
        roles_warning,
        review: detail.review,
        version: detail.version,
    }
}

fn split_roles_result(
    result: Result<RolesTable, RolesError>,
) -> (Option<RolesTable>, Option<String>) {
    match result {
        Ok(table) => (Some(table), None),
        Err(err) => (None, Some(err.to_string())),
    }
}

pub(super) fn map_access_error(err: StoreAccessError) -> DaemonMessage {
    match err {
        StoreAccessError::Disabled => unsupported_msg(),
        StoreAccessError::Failed(join_err) => {
            tracing::warn!(error = %join_err, "plan store access task failed");
            internal_msg()
        }
    }
}

pub(super) fn permit_error_msg(err: PermitError) -> DaemonMessage {
    match err {
        PermitError::AuthFailed => auth_failed_msg(),
        PermitError::PlanNotFound => plan_not_found_msg(),
        PermitError::NotPlanOwner => not_plan_owner_msg(),
        PermitError::InvalidState => invalid_state_msg(),
        PermitError::WindowNotFound => window_not_found_msg(),
        PermitError::PlanNotApproved => plan_not_approved_msg(),
    }
}

fn invalid_argument_msg(errors: &[plan::PlanError]) -> DaemonMessage {
    DaemonMessage::Error {
        code: "invalid_argument".to_string(),
        message: plan_error_message(errors),
        rejected_argument: Some(plan_error_argument(errors).to_string()),
    }
}

/// `title` when any violation is [`plan::PlanError::InvalidTitle`], `tasks`
/// for every other violation.
fn plan_error_argument(errors: &[plan::PlanError]) -> &'static str {
    if errors
        .iter()
        .any(|err| matches!(err, plan::PlanError::InvalidTitle))
    {
        "title"
    } else {
        "tasks"
    }
}

fn plan_error_message(errors: &[plan::PlanError]) -> String {
    errors
        .iter()
        .map(plan_error_detail)
        .collect::<Vec<_>>()
        .join("; ")
}

/// `err`'s own [`std::fmt::Display`], except for
/// [`plan::PlanError::WriteConflict`]: its `Display` reports only a count,
/// so this spells out each colliding pair instead -- both task ids and the
/// normalized path are already-validated values, safe to echo (module doc
/// on [`plan::PlanError`]).
fn plan_error_detail(err: &plan::PlanError) -> String {
    match err {
        plan::PlanError::WriteConflict { conflicts } => conflicts
            .iter()
            .map(|conflict| {
                format!(
                    "{} and {} both write {}",
                    conflict.task_a, conflict.task_b, conflict.path
                )
            })
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

pub(super) fn auth_failed_msg() -> DaemonMessage {
    error_msg(
        "auth_failed",
        "this connection is not authorized for the plan ledger",
    )
}

pub(super) fn plan_not_found_msg() -> DaemonMessage {
    error_msg(
        "plan_not_found",
        "no plan with this id is visible to the caller",
    )
}

fn not_plan_owner_msg() -> DaemonMessage {
    error_msg(
        "not_plan_owner",
        "the caller is not this plan's authorized owner",
    )
}

fn plan_not_approved_msg() -> DaemonMessage {
    error_msg(
        "plan_not_approved",
        "the operator has not approved this plan yet; wait for the [yagura plan] notice or \
         check review with get_plan, then open the task. Do not open a window by other means, \
         such as open_window, to do the task.",
    )
}

pub(super) fn invalid_state_msg() -> DaemonMessage {
    error_msg(
        "invalid_state",
        "the window is not in a state that allows this operation",
    )
}

pub(super) fn task_window_alive_msg() -> DaemonMessage {
    error_msg(
        "invalid_state",
        "the task already has a live window bound to it",
    )
}

pub(super) fn task_unassigned_msg() -> DaemonMessage {
    error_msg("invalid_state", "the task has no resolvable assignment")
}

pub(super) fn unsupported_msg() -> DaemonMessage {
    error_msg(
        "unsupported",
        "the plan ledger is not available on this daemon",
    )
}

fn resource_exhausted_msg() -> DaemonMessage {
    error_msg(
        "resource_exhausted",
        "plan creation was refused: a resource limit is already reached",
    )
}

pub(super) fn internal_msg() -> DaemonMessage {
    error_msg(
        "internal",
        "an internal error occurred while accessing the plan ledger",
    )
}
