//! Proof-of-authorization values for the plan ledger, and the per-plan lock
//! table that orders every change against one plan.
//!
//! A permit here is never built from a caller-supplied fact: every `issue_*`
//! function reads the real facts it needs (the plan's owner and task
//! bindings from [`PlanStore`], a window's liveness from
//! [`WindowRegistry::liveness`]) at the moment it decides, so an answer given
//! for one call can never be replayed as though it still held for a later
//! one. A permit's fields are private and built nowhere outside this module,
//! so a write that needs one can only be reached once the check that grants
//! it has actually run.
//!
//! A change permit additionally owns the plan's lock guard, rather than
//! borrowing it: the daemon's one door to a store-holding mutex moves its
//! whole closure onto a blocking thread, which requires the value it moves
//! to be `Send + 'static` -- a borrowed guard could not cross that boundary,
//! so the permit has to carry the guard itself. The lock also fixes which
//! plan a change permit targets: nothing here accepts a plan id as a
//! separate argument for a change or a nomination, only the guard already
//! scoped to one.

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::control::RequestOrigin;
use crate::plan_store::{PlanReview, PlanStore};
use crate::pty::lock_recover;
use crate::window::{WindowLiveness, WindowRegistry};

/// Which kind of plan-changing request an [`issue_change`] call is judging.
/// A change permit is tied to the kind it was issued for the same way the
/// daemon's destructive-tool permits are tied to their own tool: one
/// licence, one operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// Replacing a plan's whole task list.
    UpdatePlan,
    /// Binding a task to the window that was opened for it.
    OpenTaskWindow,
    /// Removing a plan entirely.
    DeletePlan,
    /// Setting a task's user override.
    SetTaskAssignment,
    /// Clearing a task's user override.
    ClearTaskAssignment,
    /// Moving a plan that waits for the operator to approved.
    ApprovePlan,
    /// Clearing a plan's marks on the tasks that changed since the operator
    /// last looked.
    MarkPlanSeen,
}

impl ChangeKind {
    /// Whether this kind may only ever be authorized for an approver: a
    /// window can never hold this licence, regardless of its relation to the
    /// plan or whether the plan exists at all -- existence must never be
    /// revealed by which refusal comes back.
    fn is_approver_only(self) -> bool {
        matches!(
            self,
            ChangeKind::SetTaskAssignment
                | ChangeKind::ClearTaskAssignment
                | ChangeKind::ApprovePlan
                | ChangeKind::MarkPlanSeen
        )
    }
}

/// Why an `issue_*` call in this module refused to grant a permit. Every
/// variant carries no data, so a refusal can never echo a plan id, a window
/// id or any other caller-supplied value back through [`std::fmt::Display`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PermitError {
    /// The connection never identified as a window or an approver.
    #[error("the caller is not authorized for this operation")]
    AuthFailed,
    /// No plan with the given id exists, or the calling window has no
    /// relation to it -- the same answer either way, so the two cases stay
    /// indistinguishable to the caller.
    #[error("no plan with this id is visible to the caller")]
    PlanNotFound,
    /// The calling window has a relation to the plan (it is the owner, just
    /// not currently alive, or it is bound to one of the plan's tasks) but
    /// is not authorized for this particular change.
    #[error("the caller is not this plan's authorized owner")]
    NotPlanOwner,
    /// The window this permit was requested on behalf of cannot take the
    /// request right now (its child has already exited, or it was never
    /// registered at all).
    #[error("the window is not in a state that allows this operation")]
    InvalidState,
    /// A nomination named a candidate window this registry has never opened.
    #[error("the target window is not registered")]
    WindowNotFound,
    /// The plan's alive owner asked to open a task window, but the operator
    /// has not approved the plan yet.
    #[error("the operator has not approved this plan yet")]
    PlanNotApproved,
}

/// Proof that the calling window is alive, carrying the facts
/// [`crate::plan_store::PlanStore::create_plan`] needs and does not accept
/// as separate arguments: the window that will own the new plan, and its
/// cwd.
#[derive(Debug)]
pub struct CreatePermit {
    owner_window: String,
    cwd: String,
}

impl CreatePermit {
    /// The window a plan created under this permit will be owned by.
    pub(crate) fn owner_window(&self) -> &str {
        &self.owner_window
    }

    /// The cwd a plan created under this permit is stamped with.
    pub(crate) fn cwd(&self) -> &str {
        &self.cwd
    }
}

/// Proof that a change to one plan is authorized, plus the lock guard that
/// authorization was decided under. `Send + 'static` (an
/// [`OwnedMutexGuard`] carries no borrow), so it can be moved into a
/// `spawn_blocking` closure and still license the write it was issued for.
#[derive(Debug)]
pub struct ChangePermit {
    plan_id: String,
    _guard: OwnedMutexGuard<()>,
}

impl ChangePermit {
    /// The plan this permit licenses a change to, decided from the lock
    /// guard [`issue_change`] consumed to build it -- never a separately
    /// supplied id.
    pub(crate) fn plan_id(&self) -> &str {
        &self.plan_id
    }
}

/// Proof that reading one plan is authorized. Reading never checks
/// liveness, so this carries no guard.
#[derive(Debug)]
pub struct ReadPermit {
    plan_id: String,
}

impl ReadPermit {
    /// The plan this permit licenses reading.
    pub(crate) fn plan_id(&self) -> &str {
        &self.plan_id
    }
}

/// Proof that nominating a new owner for one plan is authorized, plus the
/// lock guard the nomination was decided under.
#[derive(Debug)]
pub struct NominatePermit {
    plan_id: String,
    new_owner_window: String,
    _guard: OwnedMutexGuard<()>,
}

impl NominatePermit {
    /// The plan this permit licenses a new owner for.
    pub(crate) fn plan_id(&self) -> &str {
        &self.plan_id
    }

    /// The window being nominated as the plan's new owner.
    pub(crate) fn new_owner_window(&self) -> &str {
        &self.new_owner_window
    }
}

/// Proof that a report against one task is authorized, plus the lock guard
/// authorization was decided under. `Send + 'static`, the same reason
/// [`ChangePermit`] is: it must survive a move into a `spawn_blocking`
/// closure and still license the write it was issued for.
#[derive(Debug)]
pub struct ReportPermit {
    plan_id: String,
    task_id: String,
    _guard: OwnedMutexGuard<()>,
}

impl ReportPermit {
    /// The plan this permit licenses a report against.
    pub(crate) fn plan_id(&self) -> &str {
        &self.plan_id
    }

    /// The task this permit licenses a report against.
    pub(crate) fn task_id(&self) -> &str {
        &self.task_id
    }
}

/// Proof that recording a task's instruction-notice state is authorized,
/// plus the lock guard authorization was decided under. Daemon-internal:
/// issued to the notice-delivery side, never to a connection. `Send +
/// 'static` for the same reason [`ReportPermit`] is.
#[derive(Debug)]
pub struct NoticePermit {
    plan_id: String,
    task_id: String,
    _guard: OwnedMutexGuard<()>,
}

impl NoticePermit {
    /// The plan this permit licenses a notice-state write against.
    pub(crate) fn plan_id(&self) -> &str {
        &self.plan_id
    }

    /// The task this permit licenses a notice-state write against.
    pub(crate) fn task_id(&self) -> &str {
        &self.task_id
    }
}

/// Held by [`LockTable::lock`] and consumed by [`issue_change`] /
/// [`issue_nominate`], which read the plan id off it rather than accepting
/// one as a separate argument -- the plan a change or a nomination targets
/// is exactly the one whose lock the caller is already holding.
#[derive(Debug)]
pub struct PlanGuard {
    plan_id: String,
    guard: OwnedMutexGuard<()>,
}

/// One asynchronous lock per plan id, so that two changes to the same plan
/// never interleave while changes to different plans never wait on one
/// another. An entry is created the first time a plan is locked and removed
/// only by an explicit [`LockTable::remove`] (typically once the plan itself
/// is deleted) -- releasing every guard on an entry does not shrink the
/// table by itself.
#[derive(Debug, Default)]
pub struct LockTable {
    locks: std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<AsyncMutex<()>>>>,
}

impl LockTable {
    /// An empty lock table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquires `plan_id`'s lock, waiting if another guard on the same id is
    /// already held. Different plan ids never wait on one another.
    pub async fn lock(&self, plan_id: &str) -> PlanGuard {
        let entry = {
            let mut locks = lock_recover(&self.locks);
            locks
                .entry(plan_id.to_string())
                .or_insert_with(|| std::sync::Arc::new(AsyncMutex::new(())))
                .clone()
        };
        let guard = entry.lock_owned().await;
        PlanGuard {
            plan_id: plan_id.to_string(),
            guard,
        }
    }

    /// Removes `plan_id`'s entry, shrinking the table. Safe to call whether
    /// or not the id currently has any guard outstanding: an outstanding
    /// [`OwnedMutexGuard`] keeps its own `Arc` alive regardless of whether
    /// the table still names it.
    pub fn remove(&self, plan_id: &str) {
        lock_recover(&self.locks).remove(plan_id);
    }

    /// The number of plan ids this table currently has an entry for.
    #[must_use]
    pub fn len(&self) -> usize {
        lock_recover(&self.locks).len()
    }

    /// Whether this table currently has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A calling window's relation to one plan, read fresh on every call from
/// [`PlanStore::get_plan`] and [`WindowRegistry::liveness`] -- never cached
/// from an earlier decision, so a window that was the alive owner a moment
/// ago and has since exited is judged by what is true now.
enum Relation {
    /// The window is this plan's recorded owner.
    Owner {
        /// Whether the owner window can currently take a request.
        alive: bool,
        /// Whether the plan still waits for the operator's approval. Read
        /// from the same [`PlanStore::get_plan`] call as the ownership, so
        /// the two facts can never come from different moments.
        pending: bool,
    },
    /// The window is bound to one of the plan's tasks, but is not the owner.
    Bound,
    /// The plan does not exist, or the window has no relation to it -- one
    /// value for both, so a caller can never tell them apart.
    OutOfRange,
}

/// Derives `window_id`'s relation to `plan_id`. A store read failure is
/// treated the same as the plan not existing (fail-closed): a failure to
/// read the facts a grant depends on must never license one.
fn relation_for(
    store: &PlanStore,
    registry: &WindowRegistry,
    plan_id: &str,
    window_id: &str,
) -> Relation {
    let Ok(Some(detail)) = store.get_plan(plan_id) else {
        return Relation::OutOfRange;
    };
    if detail.owner_window.as_deref() == Some(window_id) {
        let alive = matches!(registry.liveness(window_id), WindowLiveness::Alive(_));
        let pending = detail.review == Some(PlanReview::Pending);
        return Relation::Owner { alive, pending };
    }
    if detail
        .tasks
        .iter()
        .any(|task| task.window_id.as_deref() == Some(window_id))
    {
        return Relation::Bound;
    }
    Relation::OutOfRange
}

/// Issues a [`CreatePermit`] for `origin`. Only a window whose child can
/// currently take a request may create a plan; a window this registry never
/// opened is refused the same way an already-dead one is, so the two are
/// never distinguishable to the caller.
///
/// # Errors
///
/// [`PermitError::AuthFailed`] for an unidentified connection or an
/// approver, [`PermitError::InvalidState`] for a window that cannot
/// currently take a request.
pub fn issue_create(
    origin: &RequestOrigin,
    registry: &WindowRegistry,
) -> Result<CreatePermit, PermitError> {
    let RequestOrigin::Window(window_id) = origin else {
        return Err(PermitError::AuthFailed);
    };
    match registry.liveness(window_id) {
        WindowLiveness::Alive(entry) => Ok(CreatePermit {
            owner_window: window_id.clone(),
            cwd: entry.cwd().to_string(),
        }),
        WindowLiveness::Dead | WindowLiveness::NotFound => Err(PermitError::InvalidState),
    }
}

/// Issues a [`ChangePermit`] for `kind` on the plan `guard` already locks.
///
/// Order of decision: origin first (an unidentified connection is always
/// refused, and a window can never hold a licence for an approver-only
/// kind, whatever the plan's state); then, for a window's request against a
/// structural or delete kind, the plan's existence and the window's
/// relation to it; an approver's request against a structural kind is
/// refused the same way a bound non-owner is, without needing to look the
/// plan up. Whether the plan waits for the operator is looked at only once
/// the window is known to be its alive owner, so a window that could not act
/// on the plan anyway never learns its review state.
///
/// # Errors
///
/// [`PermitError::AuthFailed`] for an unidentified connection, or a window
/// requesting an approver-only kind. [`PermitError::PlanNotFound`] when the
/// plan does not exist or the window has no relation to it.
/// [`PermitError::NotPlanOwner`] when the window is related to the plan but
/// not currently its alive owner, or when an approver requests a
/// structural kind. [`PermitError::PlanNotApproved`] when the alive owner
/// requests [`ChangeKind::OpenTaskWindow`] on a plan still waiting for the
/// operator's approval.
pub fn issue_change(
    origin: &RequestOrigin,
    kind: ChangeKind,
    guard: PlanGuard,
    store: &PlanStore,
    registry: &WindowRegistry,
) -> Result<ChangePermit, PermitError> {
    let PlanGuard { plan_id, guard } = guard;
    match origin {
        RequestOrigin::External => Err(PermitError::AuthFailed),
        RequestOrigin::Approver => {
            let approver_may_act =
                kind.is_approver_only() || matches!(kind, ChangeKind::DeletePlan);
            if approver_may_act {
                Ok(ChangePermit {
                    plan_id,
                    _guard: guard,
                })
            } else {
                Err(PermitError::NotPlanOwner)
            }
        }
        RequestOrigin::Window(window_id) => {
            if kind.is_approver_only() {
                return Err(PermitError::AuthFailed);
            }
            match relation_for(store, registry, &plan_id, window_id) {
                Relation::Owner {
                    alive: true,
                    pending: true,
                } if kind == ChangeKind::OpenTaskWindow => Err(PermitError::PlanNotApproved),
                Relation::Owner { alive: true, .. } => Ok(ChangePermit {
                    plan_id,
                    _guard: guard,
                }),
                Relation::Owner { alive: false, .. } | Relation::Bound => {
                    Err(PermitError::NotPlanOwner)
                }
                Relation::OutOfRange => Err(PermitError::PlanNotFound),
            }
        }
    }
}

/// Issues a [`ReadPermit`] for reading `plan_id`. Unlike a change, reading
/// never checks liveness: a plan's dead owner can still read its own plan,
/// and so can any window bound to one of its tasks.
///
/// # Errors
///
/// [`PermitError::AuthFailed`] for an unidentified connection.
/// [`PermitError::PlanNotFound`] when the plan does not exist or the window
/// has no relation to it.
pub fn issue_read(
    origin: &RequestOrigin,
    plan_id: &str,
    store: &PlanStore,
    registry: &WindowRegistry,
) -> Result<ReadPermit, PermitError> {
    match origin {
        RequestOrigin::External => Err(PermitError::AuthFailed),
        RequestOrigin::Approver => Ok(ReadPermit {
            plan_id: plan_id.to_string(),
        }),
        RequestOrigin::Window(window_id) => {
            match relation_for(store, registry, plan_id, window_id) {
                Relation::Owner { .. } | Relation::Bound => Ok(ReadPermit {
                    plan_id: plan_id.to_string(),
                }),
                Relation::OutOfRange => Err(PermitError::PlanNotFound),
            }
        }
    }
}

/// Issues a [`NominatePermit`] naming `new_owner_window` as the plan `guard`
/// already locks, for `origin`. Nominating is approver-only, checked before
/// anything else so a window's request is refused the same way whether or
/// not the plan exists.
///
/// # Errors
///
/// [`PermitError::AuthFailed`] for any origin other than an approver.
/// [`PermitError::InvalidState`] when `new_owner_window` was opened but
/// cannot currently take a request. [`PermitError::WindowNotFound`] when
/// `new_owner_window` names an id this registry never opened.
pub fn issue_nominate(
    origin: &RequestOrigin,
    guard: PlanGuard,
    new_owner_window: &str,
    registry: &WindowRegistry,
) -> Result<NominatePermit, PermitError> {
    if !matches!(origin, RequestOrigin::Approver) {
        return Err(PermitError::AuthFailed);
    }
    let PlanGuard { plan_id, guard } = guard;
    match registry.liveness(new_owner_window) {
        WindowLiveness::Alive(_) => Ok(NominatePermit {
            plan_id,
            new_owner_window: new_owner_window.to_string(),
            _guard: guard,
        }),
        WindowLiveness::Dead => Err(PermitError::InvalidState),
        WindowLiveness::NotFound => Err(PermitError::WindowNotFound),
    }
}

/// Issues a [`ReportPermit`] for `task_id`, within the plan `guard` already
/// locks, on behalf of `origin`. Granted only when `origin` is a window and
/// that window is `task_id`'s *current* window binding, read fresh from
/// `store` at the moment of the call: a task this plan does
/// not have, a task with no binding at all, and a window whose binding has
/// since moved to a different task_id are all refused the same way. The
/// reporting window's own liveness is never checked here -- the connection
/// making this call is alive by construction, unlike [`issue_change`]'s
/// owner check, which judges a window found only by a stored id.
///
/// # Errors
///
/// [`PermitError::AuthFailed`] for any origin other than a window.
/// [`PermitError::PlanNotFound`] when the plan `guard` locks no longer
/// exists (the caller then drops that plan's lock-table entry).
/// [`PermitError::InvalidState`] when `task_id` is not this plan's, is not
/// currently bound to the calling window, or the plan could not be read.
pub fn issue_report(
    origin: &RequestOrigin,
    task_id: &str,
    guard: PlanGuard,
    store: &PlanStore,
) -> Result<ReportPermit, PermitError> {
    let PlanGuard { plan_id, guard } = guard;
    let RequestOrigin::Window(window_id) = origin else {
        return Err(PermitError::AuthFailed);
    };
    match check_task_binding(store, &plan_id, task_id, window_id) {
        TaskBinding::Bound => Ok(ReportPermit {
            plan_id,
            task_id: task_id.to_string(),
            _guard: guard,
        }),
        TaskBinding::PlanNotFound => Err(PermitError::PlanNotFound),
        TaskBinding::NotBound => Err(PermitError::InvalidState),
    }
}

/// Issues a [`NoticePermit`] for `task_id`, within the plan `guard` already
/// locks, for a delivery to `window_id`. Granted only while `task_id` is
/// bound to `window_id` right now ([`check_task_binding`]), so a delivery
/// still holding a window the task has since been rebound away from can
/// never overwrite the new binding's state. Takes no [`RequestOrigin`]: the
/// caller is the daemon itself, with no connection to authenticate.
///
/// # Errors
///
/// [`PermitError::PlanNotFound`] when the plan `guard` locks no longer
/// exists. [`PermitError::InvalidState`] when `task_id` is not this plan's,
/// is not currently bound to `window_id`, or the plan could not be read.
pub fn issue_notice(
    window_id: &str,
    task_id: &str,
    guard: PlanGuard,
    store: &PlanStore,
) -> Result<NoticePermit, PermitError> {
    let PlanGuard { plan_id, guard } = guard;
    match check_task_binding(store, &plan_id, task_id, window_id) {
        TaskBinding::Bound => Ok(NoticePermit {
            plan_id,
            task_id: task_id.to_string(),
            _guard: guard,
        }),
        TaskBinding::PlanNotFound => Err(PermitError::PlanNotFound),
        TaskBinding::NotBound => Err(PermitError::InvalidState),
    }
}

/// What [`check_task_binding`] found for one task and one window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskBinding {
    /// The task exists and is currently bound to the window.
    Bound,
    /// The plan exists, but the task is gone, unbound, or bound to another
    /// window -- or the plan could not be read at all (fail-closed).
    NotBound,
    /// The plan no longer exists.
    PlanNotFound,
}

/// Whether `plan_id`'s task `task_id` is bound to `window_id` right now, read
/// fresh from `store`. The one place this judgement lives:
/// [`issue_report`] grants on it, and the daemon's lock-free parent lookup
/// reads it before trusting [`resolve_owner_parent`]. [`issue_notice`] grants
/// on it too.
#[must_use]
pub fn check_task_binding(
    store: &PlanStore,
    plan_id: &str,
    task_id: &str,
    window_id: &str,
) -> TaskBinding {
    let detail = match store.get_plan(plan_id) {
        Ok(Some(detail)) => detail,
        Ok(None) => return TaskBinding::PlanNotFound,
        // A read failure must neither license a report nor look like a
        // deleted plan, whose lock-table entry the caller would then drop.
        Err(_) => return TaskBinding::NotBound,
    };
    let bound = detail
        .tasks
        .into_iter()
        .find(|task| task.task_id == task_id)
        .is_some_and(|task| task.window_id.as_deref() == Some(window_id));
    if bound {
        TaskBinding::Bound
    } else {
        TaskBinding::NotBound
    }
}

/// The "task-bound window -> plan owner, alive only" half of the full
/// parent-derivation rule: `plan_id`'s recorded
/// owner, but only when that owner is currently alive. A dead or
/// never-registered owner resolves to no parent, even though the plan still
/// names it. A caller needing the fuller rule (the `task_origin` binding
/// check, the opener fallback) builds it on top of this function and
/// [`check_task_binding`] rather than duplicating either check.
#[must_use]
pub fn resolve_owner_parent(
    store: &PlanStore,
    registry: &WindowRegistry,
    plan_id: &str,
) -> Option<String> {
    let owner = store.get_plan(plan_id).ok().flatten()?.owner_window?;
    matches!(registry.liveness(&owner), WindowLiveness::Alive(_)).then_some(owner)
}
