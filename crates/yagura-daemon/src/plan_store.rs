//! Persistence port for the plan ledger: `plan`, `plan_task`, `plan_task_dep`
//! and `plan_task_write` (schema v8: `migrations/007_plans.sql` plus the
//! task-report columns `migrations/008_task_report.sql` adds to
//! `plan_task`).
//!
//! Every write method here takes a permit from [`crate::plan_permit`] rather
//! than a plan id, an owner window id or a cwd as a separate argument: the
//! permit is where those facts already live, once authorization decided
//! them. Reading is the one exception -- [`PlanStore::get_plan`] and
//! [`PlanStore::list_plans`] take no permit at all, since this module never
//! judges authorization itself; a caller reaches them only once
//! [`crate::plan_permit::issue_read`] granted a [`crate::plan_permit::ReadPermit`],
//! whose `plan_id` accessor is this crate's only door to that decision.
//!
//! Validating a [`crate::plan::PlanSpec`] is not this module's job either --
//! every method here takes an already-[`crate::plan::validate_plan`]-ed
//! [`crate::plan::ValidatedPlan`].

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

use crate::plan::{Assignment, ValidatedPlan, ValidatedTask};
use crate::plan_permit::{ChangePermit, CreatePermit, NominatePermit, NoticePermit, ReportPermit};
use crate::sql::FixedSql;
use crate::store::{self, StoreError};

/// Whether a plan waits for the operator, and which of its tasks changed
/// since the operator last looked. Reused from the wire directly, the same
/// way [`PlanTaskReport::outcome`] is: storage and the wire carry the same
/// closed value sets, so there is nothing for this crate to translate.
pub use yagura_protocol::{PlanReview, ReviewMark};

/// Overall cap on the number of plans one store may hold at once.
pub const MAX_TOTAL_PLANS: i64 = 64;

/// Cap on the number of plans a single owner window may hold at once.
pub const MAX_PLANS_PER_OWNER: i64 = 16;

/// Random bytes drawn for a plan id's suffix (128 bits), the same width
/// [`crate::token::StatusToken`] draws for its own random value.
const PLAN_ID_RANDOM_BYTES: usize = 16;

/// Which cap [`PlanStoreError::ResourceExhausted`] was refused by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceLimitKind {
    /// [`MAX_TOTAL_PLANS`] is already reached.
    TotalPlans,
    /// [`MAX_PLANS_PER_OWNER`] is already reached for the creating window.
    OwnerPlans,
}

/// Errors [`PlanStore::create_plan`] returns beyond a plain [`StoreError`].
#[derive(Debug, Error)]
pub enum PlanStoreError {
    /// Any underlying storage failure.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Creating the plan was refused because it would exceed one of the
    /// bounds in [`ResourceLimitKind`]. No row was written.
    #[error("plan creation was refused: the resource limit is already reached")]
    ResourceExhausted(ResourceLimitKind),
}

/// A resolved profile/model/effort triple as stored, mirroring
/// [`crate::plan::Assignment`] but reconstructed from the three nullable
/// columns a task row carries for the parent and the user each.
fn assignment_from_columns(
    profile: Option<String>,
    model: Option<String>,
    effort: Option<String>,
) -> Option<Assignment> {
    profile.map(|profile| Assignment {
        profile,
        model,
        effort,
    })
}

/// The three columns one [`Assignment`] (or its absence) is stored as.
fn assignment_to_columns(
    assignment: Option<&Assignment>,
) -> (Option<String>, Option<String>, Option<String>) {
    match assignment {
        Some(assignment) => (
            Some(assignment.profile.clone()),
            assignment.model.clone(),
            assignment.effort.clone(),
        ),
        None => (None, None, None),
    }
}

/// The latest report recorded against one task, as stored (schema v8's four
/// `report_*` columns on `plan_task`). `outcome` reuses
/// [`yagura_protocol::ReportOutcome`] directly: the wire and the storage
/// representation are the same two-value set, so there is nothing for this
/// crate to translate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanTaskReport {
    /// Whether the work succeeded.
    pub outcome: yagura_protocol::ReportOutcome,
    /// The reporting window's account of what happened.
    pub summary: String,
    /// Where a longer result was written, if the reporter named a file.
    pub path: Option<String>,
    /// When the report was recorded, in the same epoch-seconds unit
    /// `plan.created_at` / `plan.updated_at` use.
    pub reported_at: i64,
}

/// `plan_task.report_outcome`'s stored text for `outcome`. The CHECK
/// constraint `migrations/008_task_report.sql` adds refuses any other value.
fn report_outcome_to_column(outcome: yagura_protocol::ReportOutcome) -> &'static str {
    match outcome {
        yagura_protocol::ReportOutcome::Succeeded => "succeeded",
        yagura_protocol::ReportOutcome::Failed => "failed",
    }
}

/// The inverse of [`report_outcome_to_column`]. `None` for anything besides
/// the two values the CHECK constraint allows -- defensive against a
/// database written by a build this one cannot fully trust, even though the
/// constraint should make that unreachable in practice.
fn report_outcome_from_column(value: &str) -> Option<yagura_protocol::ReportOutcome> {
    match value {
        "succeeded" => Some(yagura_protocol::ReportOutcome::Succeeded),
        "failed" => Some(yagura_protocol::ReportOutcome::Failed),
        _ => None,
    }
}

/// Rebuilds a task's report from its four nullable columns: a report exists
/// exactly when `report_outcome` decodes to a known value, the same
/// "presence of the first field decides" convention
/// [`assignment_from_columns`] uses for an assignment.
fn report_from_columns(
    outcome: Option<String>,
    summary: Option<String>,
    path: Option<String>,
    reported_at: Option<i64>,
) -> Option<PlanTaskReport> {
    let outcome = report_outcome_from_column(outcome.as_deref()?)?;
    Some(PlanTaskReport {
        outcome,
        summary: summary.unwrap_or_default(),
        path,
        reported_at: reported_at.unwrap_or(0),
    })
}

/// Where delivering one task's instruction to its bound window stands, as
/// stored (schema v10's two `instruction_notice_*` columns on `plan_task`).
/// `state` reuses [`yagura_protocol::InstructionNoticeState`] directly, the
/// same way [`PlanTaskReport::outcome`] reuses the wire's report outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanTaskInstructionNotice {
    /// How far the delivery has come.
    pub state: yagura_protocol::InstructionNoticeState,
    /// When `state` was reached, in epoch seconds. `None` while pending.
    pub at: Option<i64>,
}

/// `plan_task.instruction_notice_state`'s stored text for `state`. The CHECK
/// constraint `migrations/010_instruction_notice.sql` adds refuses any other
/// value.
fn instruction_notice_state_to_column(
    state: yagura_protocol::InstructionNoticeState,
) -> &'static str {
    match state {
        yagura_protocol::InstructionNoticeState::Pending => "pending",
        yagura_protocol::InstructionNoticeState::Delivered => "delivered",
        yagura_protocol::InstructionNoticeState::WindowClosed => "window_closed",
    }
}

/// The inverse of [`instruction_notice_state_to_column`]; `None` for any
/// value outside the CHECK constraint's set.
fn instruction_notice_state_from_column(
    value: &str,
) -> Option<yagura_protocol::InstructionNoticeState> {
    match value {
        "pending" => Some(yagura_protocol::InstructionNoticeState::Pending),
        "delivered" => Some(yagura_protocol::InstructionNoticeState::Delivered),
        "window_closed" => Some(yagura_protocol::InstructionNoticeState::WindowClosed),
        _ => None,
    }
}

/// Rebuilds a task's instruction notice from its two nullable columns: a
/// notice exists exactly when the state column decodes to a known value.
fn instruction_notice_from_columns(
    state: Option<String>,
    at: Option<i64>,
) -> Option<PlanTaskInstructionNotice> {
    let state = instruction_notice_state_from_column(state.as_deref()?)?;
    Some(PlanTaskInstructionNotice { state, at })
}

/// `plan.review`'s stored text for `review`. The CHECK constraint
/// `migrations/012_plan_review.sql` adds refuses any other value.
fn review_to_column(review: PlanReview) -> &'static str {
    match review {
        PlanReview::Pending => "pending",
        PlanReview::Approved => "approved",
    }
}

/// The inverse of [`review_to_column`]; `None` for any value outside the
/// CHECK constraint's set.
fn review_from_column(value: Option<&str>) -> Option<PlanReview> {
    match value? {
        "pending" => Some(PlanReview::Pending),
        "approved" => Some(PlanReview::Approved),
        _ => None,
    }
}

/// `plan_task.review_mark`'s stored text for `mark`, under the same CHECK
/// constraint as [`review_to_column`].
fn review_mark_to_column(mark: ReviewMark) -> &'static str {
    match mark {
        ReviewMark::New => "new",
        ReviewMark::Changed => "changed",
    }
}

/// The inverse of [`review_mark_to_column`].
fn review_mark_from_column(value: Option<&str>) -> Option<ReviewMark> {
    match value? {
        "new" => Some(ReviewMark::New),
        "changed" => Some(ReviewMark::Changed),
        _ => None,
    }
}

/// `plan.version` as stored (a non-negative `INTEGER`) widened to the wire's
/// unsigned width.
fn version_from_column(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// What [`PlanStore::approve_plan`] found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApproveOutcome {
    /// The plan was pending at the given version and is now approved.
    Approved,
    /// The plan was already approved; nothing was written.
    AlreadyApproved,
    /// The plan was created without review, so there is nothing to approve.
    NotReviewable,
    /// The plan's task list changed since the given version was read.
    VersionMismatch,
    /// The plan no longer exists.
    PlanGone,
}

/// What [`PlanStore::clear_review_marks`] found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearOutcome {
    /// Every task's mark was cleared.
    Cleared,
    /// The plan's task list changed since the given version was read; every
    /// mark was left in place.
    VersionMismatch,
    /// The plan no longer exists.
    PlanGone,
}

/// One task as read back by [`PlanStore::get_plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanTaskRecord {
    /// Identifier, unique within the plan.
    pub task_id: String,
    /// Registration order, `0`-based.
    pub seq: i64,
    /// Display title.
    pub title: String,
    /// Role name, unresolved.
    pub role: Option<String>,
    /// `task_id`s this task depends on, in insertion order.
    pub depends_on: Vec<String>,
    /// Normalized write-target paths, in their original case, in insertion
    /// order.
    pub writes: Vec<String>,
    /// The parent-specified assignment, if any.
    pub parent: Option<Assignment>,
    /// The user override, if any.
    pub user: Option<Assignment>,
    /// The window bound to this task, if any.
    pub window_id: Option<String>,
    /// The latest report recorded against this task, if any.
    pub report: Option<PlanTaskReport>,
    /// The task's instruction text, if any.
    pub instruction: Option<String>,
    /// Where delivering `instruction` to the bound window stands, if the
    /// task has been bound while carrying one.
    pub instruction_notice: Option<PlanTaskInstructionNotice>,
    /// Whether this task was added or changed since the operator last looked
    /// at an approved plan.
    pub review_mark: Option<ReviewMark>,
}

/// One plan as read back by [`PlanStore::get_plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDetail {
    /// Identifier.
    pub plan_id: String,
    /// Display title.
    pub title: String,
    /// The creating window's cwd at the time the plan was created.
    pub cwd: String,
    /// The plan's current owner window, if any.
    pub owner_window: Option<String>,
    /// Tasks, in registration order.
    pub tasks: Vec<PlanTaskRecord>,
    /// Whether the plan waits for, or has had, the operator's approval.
    /// `None` for a plan created without review.
    pub review: Option<PlanReview>,
    /// How many times the task list has been replaced since creation.
    pub version: u64,
}

/// One plan as read back by [`PlanStore::list_plans`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSummary {
    /// Identifier.
    pub plan_id: String,
    /// Display title.
    pub title: String,
    /// The plan's current owner window, if any.
    pub owner_window: Option<String>,
    /// Number of tasks the plan currently has.
    pub task_count: i64,
    /// Same as [`PlanDetail::review`].
    pub review: Option<PlanReview>,
    /// Whether any task carries a [`ReviewMark`].
    pub unseen_changes: bool,
}

/// Lowercase hex digits [`to_lowercase_hex`] indexes into.
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Encodes `bytes` as lowercase hex, one nibble at a time.
fn to_lowercase_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Draws a fresh, hard-to-guess plan id (`p-` followed by
/// [`PLAN_ID_RANDOM_BYTES`] random bytes as lowercase hex).
///
/// A failure to obtain randomness degrades to a monotonic counter suffix
/// instead of failing the call outright -- creating a plan must never come
/// to depend on the OS RNG being available, and the counter still keeps
/// every id this process draws distinct even though it gives up on being
/// hard to guess.
fn generate_plan_id() -> String {
    let mut bytes = [0u8; PLAN_ID_RANDOM_BYTES];
    match yagura_os::fill_random(&mut bytes) {
        Ok(()) => "p-".to_string() + &to_lowercase_hex(&bytes),
        Err(err) => {
            tracing::warn!(
                "failed to obtain randomness for a plan id ({err}); falling back to a \
                 monotonic id"
            );
            let counter =
                PLAN_ID_FALLBACK_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            "p-fallback-".to_string() + &counter.to_string()
        }
    }
}

/// Backstop for [`generate_plan_id`]'s fallback branch.
static PLAN_ID_FALLBACK_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Survivor state (a surviving task_id's user override, bound window,
/// report and instruction notice) [`PlanStore::replace_tasks`] carries across
/// the delete-and-reinsert it otherwise shares with [`PlanStore::create_plan`]:
/// `(user_profile, user_model, user_effort, window_id, report_outcome,
/// report_summary, report_path, reported_at, instruction_notice_state,
/// instruction_notice_at)`, kept in their raw stored form so
/// [`insert_tasks`] can bind them straight back into the same `INSERT`.
#[allow(clippy::type_complexity)]
type SurvivorState = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<i64>,
);

/// One raw `plan_task` row, in the column order
/// [`PlanStore::get_plan`]'s own query selects them in, before it is split
/// into a [`PlanTaskRecord`]'s separate `parent` and `user` assignments.
/// Named purely to give `get_plan`'s `query_map` a readable return type --
/// every field is already documented on [`PlanTaskRecord`], the struct this
/// tuple is immediately split into.
#[allow(clippy::type_complexity)]
type TaskRow = (
    String,
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
);

/// Whether `new` differs from `old` in anything the operator reviews: the
/// title, the role, the parent's assignment, the dependencies, the write
/// targets or the instruction. The user's own override, the window binding
/// and the report are not the plan author's changes.
fn task_differs(old: &PlanTaskRecord, new: &ValidatedTask) -> bool {
    old.title != new.title
        || old.role != new.role
        || old.parent != new.parent
        || old.depends_on != new.depends_on
        || old.writes != new.writes
        || old.instruction != new.instruction
}

/// The mark `new` carries after a replace of an approved plan whose previous
/// version of the task (if any) was `old`. A task the operator has not seen
/// yet stays new however often it changes.
fn review_mark_after_replace(
    old: Option<&PlanTaskRecord>,
    new: &ValidatedTask,
) -> Option<ReviewMark> {
    let Some(old) = old else {
        return Some(ReviewMark::New);
    };
    match old.review_mark {
        Some(ReviewMark::New) => Some(ReviewMark::New),
        _ if task_differs(old, new) => Some(ReviewMark::Changed),
        previous => previous,
    }
}

/// Inserts every task in `tasks` (registration order becomes `seq`) plus its
/// dependency and write-target rows, for `plan_id`. `survivors` supplies the
/// user-override and window-binding columns a task_id already had before
/// this call -- empty for [`PlanStore::create_plan`], which has nothing to
/// carry over. `marks` supplies each task's review mark; a task it does not
/// name is stored unmarked. The instruction always comes from `tasks`
/// itself: an update that omits it clears it, and with it the task's
/// instruction notice, which has nothing left to describe.
fn insert_tasks(
    tx: &rusqlite::Transaction<'_>,
    plan_id: &str,
    tasks: &[ValidatedTask],
    survivors: &HashMap<String, SurvivorState>,
    marks: &HashMap<String, ReviewMark>,
) -> Result<(), StoreError> {
    for (index, task) in tasks.iter().enumerate() {
        let seq = i64::try_from(index).unwrap_or(i64::MAX);
        let (parent_profile, parent_model, parent_effort) =
            assignment_to_columns(task.parent.as_ref());
        let (
            user_profile,
            user_model,
            user_effort,
            window_id,
            report_outcome,
            report_summary,
            report_path,
            reported_at,
            notice_state,
            notice_at,
        ) = survivors.get(&task.task_id).cloned().unwrap_or_default();
        let (notice_state, notice_at) = if task.instruction.is_some() {
            (notice_state, notice_at)
        } else {
            (None, None)
        };
        let review_mark = marks.get(&task.task_id).copied().map(review_mark_to_column);
        tx.execute_fixed(
            "INSERT INTO plan_task \
             (plan_id, task_id, seq, title, role, parent_profile, parent_model, \
              parent_effort, user_profile, user_model, user_effort, window_id, \
              report_outcome, report_summary, report_path, reported_at, instruction, \
              instruction_notice_state, instruction_notice_at, review_mark) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
              ?18, ?19, ?20)",
            params![
                plan_id,
                task.task_id,
                seq,
                task.title,
                task.role,
                parent_profile,
                parent_model,
                parent_effort,
                user_profile,
                user_model,
                user_effort,
                window_id,
                report_outcome,
                report_summary,
                report_path,
                reported_at,
                task.instruction,
                notice_state,
                notice_at,
                review_mark
            ],
        )?;
        for dependency in &task.depends_on {
            tx.execute_fixed(
                "INSERT INTO plan_task_dep (plan_id, task_id, depends_on) VALUES (?1, ?2, ?3)",
                params![plan_id, task.task_id, dependency],
            )?;
        }
        for path in &task.writes {
            tx.execute_fixed(
                "INSERT INTO plan_task_write (plan_id, task_id, path) VALUES (?1, ?2, ?3)",
                params![plan_id, task.task_id, path],
            )?;
        }
    }
    Ok(())
}

/// One raw `plan` row as [`read_plan`] selects it: title, cwd, owner window,
/// review and version.
type PlanHeaderRow = (String, String, Option<String>, Option<String>, i64);

/// [`PlanStore::get_plan`] on any connection, so [`PlanStore::replace_tasks`]
/// can read the previous version of a plan inside its own transaction.
fn read_plan(conn: &Connection, plan_id: &str) -> Result<Option<PlanDetail>, StoreError> {
    let header: Option<PlanHeaderRow> = conn
        .query_row_fixed(
            "SELECT title, cwd, owner_window, review, version FROM plan WHERE plan_id = ?1",
            params![plan_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((title, cwd, owner_window, review, version)) = header else {
        return Ok(None);
    };

    let mut stmt = conn.prepare_fixed(
        "SELECT task_id, seq, title, role, parent_profile, parent_model, parent_effort, \
         user_profile, user_model, user_effort, window_id, \
         report_outcome, report_summary, report_path, reported_at, instruction, \
         instruction_notice_state, instruction_notice_at, review_mark \
         FROM plan_task WHERE plan_id = ?1 ORDER BY seq ASC",
    )?;
    let task_rows: Vec<TaskRow> = stmt
        .query_map(params![plan_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
                row.get(12)?,
                row.get(13)?,
                row.get(14)?,
                row.get(15)?,
                row.get(16)?,
                row.get(17)?,
                row.get(18)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut tasks = Vec::with_capacity(task_rows.len());
    for (
        task_id,
        seq,
        task_title,
        role,
        parent_profile,
        parent_model,
        parent_effort,
        user_profile,
        user_model,
        user_effort,
        window_id,
        report_outcome,
        report_summary,
        report_path,
        reported_at,
        instruction,
        notice_state,
        notice_at,
        review_mark,
    ) in task_rows
    {
        let depends_on = read_task_deps(conn, plan_id, &task_id)?;
        let writes = read_task_writes(conn, plan_id, &task_id)?;
        tasks.push(PlanTaskRecord {
            task_id,
            seq,
            title: task_title,
            role,
            depends_on,
            writes,
            parent: assignment_from_columns(parent_profile, parent_model, parent_effort),
            user: assignment_from_columns(user_profile, user_model, user_effort),
            window_id,
            report: report_from_columns(report_outcome, report_summary, report_path, reported_at),
            instruction,
            instruction_notice: instruction_notice_from_columns(notice_state, notice_at),
            review_mark: review_mark_from_column(review_mark.as_deref()),
        });
    }

    Ok(Some(PlanDetail {
        plan_id: plan_id.to_string(),
        title,
        cwd,
        owner_window,
        tasks,
        review: review_from_column(review.as_deref()),
        version: version_from_column(version),
    }))
}

/// One task's `depends_on` values, in insertion (`rowid`) order.
fn read_task_deps(
    conn: &Connection,
    plan_id: &str,
    task_id: &str,
) -> Result<Vec<String>, StoreError> {
    let mut stmt = conn.prepare_fixed(
        "SELECT depends_on FROM plan_task_dep \
         WHERE plan_id = ?1 AND task_id = ?2 ORDER BY rowid ASC",
    )?;
    let rows = stmt.query_map(params![plan_id, task_id], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<_>>()
        .map_err(StoreError::from)
}

/// One task's `writes` values, in insertion (`rowid`) order. Shares
/// [`read_task_deps`]'s shape but not its SQL text: every statement is text
/// fixed at build time ([`crate::sql::FixedSql`]), which rules out building
/// one query from a shared table/column name at run time.
fn read_task_writes(
    conn: &Connection,
    plan_id: &str,
    task_id: &str,
) -> Result<Vec<String>, StoreError> {
    let mut stmt = conn.prepare_fixed(
        "SELECT path FROM plan_task_write \
         WHERE plan_id = ?1 AND task_id = ?2 ORDER BY rowid ASC",
    )?;
    let rows = stmt.query_map(params![plan_id, task_id], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<_>>()
        .map_err(StoreError::from)
}

/// A connection to the plan ledger's own tables, migrated to schema v7. This
/// type has no query port of its own for authorization: it stores and
/// returns rows, and [`crate::plan_permit`] is what decides who may reach
/// which method.
#[derive(Debug)]
pub struct PlanStore {
    conn: Connection,
}

impl PlanStore {
    /// Opens (creating and migrating if needed) `db_path`, keeping every
    /// plan, task, override and window binding it already holds.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] the shared migration path can return.
    pub fn open(db_path: &Path) -> Result<Self, StoreError> {
        let conn = store::open_migrated(db_path)?;
        Ok(Self { conn })
    }

    /// [`Self::open`], then clears every plan's owner and every task's
    /// window binding: window ids from a previous process's registry never
    /// survive a restart, so a new boot must not let a stale id linger as
    /// though it still named something live. A still-pending instruction
    /// notice becomes window-closed, since the window it awaited is gone.
    /// Plans, tasks, dependencies, write targets and user overrides are all
    /// left untouched.
    ///
    /// # Errors
    ///
    /// Same as [`Self::open`], plus any [`StoreError`] from the clearing
    /// transaction.
    pub fn open_as_new_boot(db_path: &Path) -> Result<Self, StoreError> {
        let mut opened = Self::open(db_path)?;
        let tx = opened
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_fixed("UPDATE plan SET owner_window = NULL", [])?;
        tx.execute_fixed("UPDATE plan_task SET window_id = NULL", [])?;
        tx.execute_fixed(
            "UPDATE plan_task SET instruction_notice_state = ?1, instruction_notice_at = ?2 \
             WHERE instruction_notice_state = ?3",
            params![
                instruction_notice_state_to_column(
                    yagura_protocol::InstructionNoticeState::WindowClosed
                ),
                store::current_epoch_secs(),
                instruction_notice_state_to_column(
                    yagura_protocol::InstructionNoticeState::Pending
                )
            ],
        )?;
        tx.commit()?;
        Ok(opened)
    }

    /// Creates a plan owned by `permit`'s window, with `permit`'s cwd, from
    /// an already-validated plan, in the review state `review` (`None` for a
    /// plan that never waits for the operator). The two resource caps are
    /// enforced inside this call's own transaction, so a plan that would
    /// exceed either is refused with no row written.
    ///
    /// # Errors
    ///
    /// [`PlanStoreError::ResourceExhausted`] when creating this plan would
    /// exceed [`MAX_TOTAL_PLANS`] or [`MAX_PLANS_PER_OWNER`] for `permit`'s
    /// window. [`PlanStoreError::Store`] for any underlying storage failure.
    pub fn create_plan(
        &mut self,
        permit: &CreatePermit,
        plan: &ValidatedPlan,
        review: Option<PlanReview>,
    ) -> Result<String, PlanStoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::from)?;

        // The owner-specific cap is checked before the overall one: a window that has
        // reached its own 16-plan cap must be refused OwnerPlans even in the exact case
        // where the overall 64-plan cap is reached at the same moment (four owners each
        // at their own cap sum to the overall cap) -- the overall cap's own refusal is
        // reserved for a window still under its own cap but shut out by the total.
        let owner_count: i64 = tx
            .query_row_fixed(
                "SELECT COUNT(*) FROM plan WHERE owner_window = ?1",
                params![permit.owner_window()],
                |row| row.get(0),
            )
            .map_err(StoreError::from)?;
        if owner_count >= MAX_PLANS_PER_OWNER {
            return Err(PlanStoreError::ResourceExhausted(
                ResourceLimitKind::OwnerPlans,
            ));
        }
        let total: i64 = tx
            .query_row_fixed("SELECT COUNT(*) FROM plan", [], |row| row.get(0))
            .map_err(StoreError::from)?;
        if total >= MAX_TOTAL_PLANS {
            return Err(PlanStoreError::ResourceExhausted(
                ResourceLimitKind::TotalPlans,
            ));
        }

        let plan_id = generate_plan_id();
        let now = store::current_epoch_secs();
        tx.execute_fixed(
            "INSERT INTO plan (plan_id, title, cwd, owner_window, created_at, updated_at, \
             review) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                plan_id,
                plan.title,
                permit.cwd(),
                permit.owner_window(),
                now,
                now,
                review.map(review_to_column)
            ],
        )
        .map_err(StoreError::from)?;
        insert_tasks(&tx, &plan_id, &plan.tasks, &HashMap::new(), &HashMap::new())?;

        tx.commit().map_err(StoreError::from)?;
        Ok(plan_id)
    }

    /// Replaces `permit`'s plan's whole task list with `plan`'s tasks:
    /// surviving `task_id`s (present in both the old and the new set) keep
    /// their user override and bound window; a `task_id` absent from `plan`
    /// loses both along with the rest of its row. The new tasks'
    /// registration order becomes the new `seq` order. The plan's version
    /// advances by one on every call. On an approved plan each task is
    /// marked against its previous version (see [`ReviewMark`]); on any
    /// other plan nothing is marked.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying transaction.
    pub fn replace_tasks(
        &mut self,
        permit: &ChangePermit,
        plan: &ValidatedPlan,
    ) -> Result<(), StoreError> {
        let plan_id = permit.plan_id();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let mut marks: HashMap<String, ReviewMark> = HashMap::new();
        if let Some(previous) = read_plan(&tx, plan_id)?
            && previous.review == Some(PlanReview::Approved)
        {
            for task in &plan.tasks {
                let old = previous
                    .tasks
                    .iter()
                    .find(|old| old.task_id == task.task_id);
                if let Some(mark) = review_mark_after_replace(old, task) {
                    marks.insert(task.task_id.clone(), mark);
                }
            }
        }

        let mut survivors: HashMap<String, SurvivorState> = HashMap::new();
        {
            let mut stmt = tx.prepare_fixed(
                "SELECT task_id, user_profile, user_model, user_effort, window_id, \
                 report_outcome, report_summary, report_path, reported_at, \
                 instruction_notice_state, instruction_notice_at \
                 FROM plan_task WHERE plan_id = ?1",
            )?;
            let rows = stmt.query_map(params![plan_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<i64>>(10)?,
                ))
            })?;
            for row in rows {
                let (
                    task_id,
                    user_profile,
                    user_model,
                    user_effort,
                    window_id,
                    report_outcome,
                    report_summary,
                    report_path,
                    reported_at,
                    notice_state,
                    notice_at,
                ) = row?;
                survivors.insert(
                    task_id,
                    (
                        user_profile,
                        user_model,
                        user_effort,
                        window_id,
                        report_outcome,
                        report_summary,
                        report_path,
                        reported_at,
                        notice_state,
                        notice_at,
                    ),
                );
            }
        }

        tx.execute_fixed(
            "DELETE FROM plan_task_write WHERE plan_id = ?1",
            params![plan_id],
        )?;
        tx.execute_fixed(
            "DELETE FROM plan_task_dep WHERE plan_id = ?1",
            params![plan_id],
        )?;
        tx.execute_fixed("DELETE FROM plan_task WHERE plan_id = ?1", params![plan_id])?;

        insert_tasks(&tx, plan_id, &plan.tasks, &survivors, &marks)?;

        tx.execute_fixed(
            "UPDATE plan SET title = ?2, updated_at = ?3, version = version + 1 \
             WHERE plan_id = ?1",
            params![plan_id, plan.title, store::current_epoch_secs()],
        )?;

        tx.commit()?;
        Ok(())
    }

    /// Reads back one plan, or `None` if `plan_id` does not exist.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operations.
    pub fn get_plan(&self, plan_id: &str) -> Result<Option<PlanDetail>, StoreError> {
        read_plan(&self.conn, plan_id)
    }

    /// Every plan's summary, ordered by `plan_id`.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operations.
    pub fn list_plans(&self) -> Result<Vec<PlanSummary>, StoreError> {
        let mut stmt = self.conn.prepare_fixed(
            "SELECT plan_id, title, owner_window, \
             (SELECT COUNT(*) FROM plan_task WHERE plan_task.plan_id = plan.plan_id), \
             review, \
             EXISTS (SELECT 1 FROM plan_task WHERE plan_task.plan_id = plan.plan_id \
             AND review_mark IS NOT NULL) \
             FROM plan ORDER BY plan_id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(PlanSummary {
                plan_id: row.get(0)?,
                title: row.get(1)?,
                owner_window: row.get(2)?,
                task_count: row.get(3)?,
                review: review_from_column(row.get::<_, Option<String>>(4)?.as_deref()),
                unseen_changes: row.get(5)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(StoreError::from)
    }

    /// Approves `permit`'s plan if it is pending and its task list is still at
    /// `version` -- the version the operator looked at, so an approval can
    /// never cover a task list the operator has not seen.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying transaction.
    pub fn approve_plan(
        &mut self,
        permit: &ChangePermit,
        version: u64,
    ) -> Result<ApproveOutcome, StoreError> {
        let plan_id = permit.plan_id();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(Option<String>, i64)> = tx
            .query_row_fixed(
                "SELECT review, version FROM plan WHERE plan_id = ?1",
                params![plan_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((review, stored_version)) = row else {
            return Ok(ApproveOutcome::PlanGone);
        };
        match review_from_column(review.as_deref()) {
            None => return Ok(ApproveOutcome::NotReviewable),
            Some(PlanReview::Approved) => return Ok(ApproveOutcome::AlreadyApproved),
            Some(PlanReview::Pending) => {}
        }
        if version_from_column(stored_version) != version {
            return Ok(ApproveOutcome::VersionMismatch);
        }
        tx.execute_fixed(
            "UPDATE plan SET review = ?2, updated_at = ?3 WHERE plan_id = ?1",
            params![
                plan_id,
                review_to_column(PlanReview::Approved),
                store::current_epoch_secs()
            ],
        )?;
        tx.commit()?;
        Ok(ApproveOutcome::Approved)
    }

    /// Clears every task's [`ReviewMark`] on `permit`'s plan, but only while
    /// its task list is still at `version`: a change the operator has not
    /// seen yet must keep its mark.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying transaction.
    pub fn clear_review_marks(
        &mut self,
        permit: &ChangePermit,
        version: u64,
    ) -> Result<ClearOutcome, StoreError> {
        let plan_id = permit.plan_id();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored_version: Option<i64> = tx
            .query_row_fixed(
                "SELECT version FROM plan WHERE plan_id = ?1",
                params![plan_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(stored_version) = stored_version else {
            return Ok(ClearOutcome::PlanGone);
        };
        if version_from_column(stored_version) != version {
            return Ok(ClearOutcome::VersionMismatch);
        }
        tx.execute_fixed(
            "UPDATE plan_task SET review_mark = NULL WHERE plan_id = ?1",
            params![plan_id],
        )?;
        tx.commit()?;
        Ok(ClearOutcome::Cleared)
    }

    /// Deletes `permit`'s plan and every row it owns across the three child
    /// tables.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying transaction.
    pub fn delete_plan(&mut self, permit: ChangePermit) -> Result<(), StoreError> {
        let plan_id = permit.plan_id();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_fixed(
            "DELETE FROM plan_task_write WHERE plan_id = ?1",
            params![plan_id],
        )?;
        tx.execute_fixed(
            "DELETE FROM plan_task_dep WHERE plan_id = ?1",
            params![plan_id],
        )?;
        tx.execute_fixed("DELETE FROM plan_task WHERE plan_id = ?1", params![plan_id])?;
        tx.execute_fixed("DELETE FROM plan WHERE plan_id = ?1", params![plan_id])?;
        tx.commit()?;
        Ok(())
    }

    /// Sets (or, passing `None`, clears) `task_id`'s user override within
    /// `permit`'s plan.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying transaction.
    pub fn set_user_override(
        &mut self,
        permit: &ChangePermit,
        task_id: &str,
        assignment: Option<Assignment>,
    ) -> Result<(), StoreError> {
        let plan_id = permit.plan_id();
        let (profile, model, effort) = assignment_to_columns(assignment.as_ref());
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_fixed(
            "UPDATE plan_task SET user_profile = ?3, user_model = ?4, user_effort = ?5 \
             WHERE plan_id = ?1 AND task_id = ?2",
            params![plan_id, task_id, profile, model, effort],
        )?;
        tx.execute_fixed(
            "UPDATE plan SET updated_at = ?2 WHERE plan_id = ?1",
            params![plan_id, store::current_epoch_secs()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Binds `task_id` (within `permit`'s plan) to `window_id`, clearing
    /// whatever report the task previously carried: a new binding means a
    /// new attempt, so a report against the task's old window must not be
    /// read as though it were made under the new one. For the same reason
    /// the instruction notice restarts at pending when the task carries an
    /// instruction, and is cleared when it carries none.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying transaction.
    pub fn set_task_window(
        &mut self,
        permit: &ChangePermit,
        task_id: &str,
        window_id: &str,
    ) -> Result<(), StoreError> {
        let plan_id = permit.plan_id();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_fixed(
            "UPDATE plan_task SET window_id = ?3, report_outcome = NULL, \
             report_summary = NULL, report_path = NULL, reported_at = NULL, \
             instruction_notice_state = CASE WHEN instruction IS NULL THEN NULL ELSE ?4 END, \
             instruction_notice_at = NULL \
             WHERE plan_id = ?1 AND task_id = ?2",
            params![
                plan_id,
                task_id,
                window_id,
                instruction_notice_state_to_column(
                    yagura_protocol::InstructionNoticeState::Pending
                )
            ],
        )?;
        tx.execute_fixed(
            "UPDATE plan SET updated_at = ?2 WHERE plan_id = ?1",
            params![plan_id, store::current_epoch_secs()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records a report against `permit`'s task: exactly the four report
    /// columns, nothing else on `plan` or `plan_task`.
    /// A second report against the same task overwrites the first outright
    /// -- there is no history to append to, only the latest
    /// state.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying transaction.
    pub fn write_task_report(
        &mut self,
        permit: &ReportPermit,
        outcome: yagura_protocol::ReportOutcome,
        summary: &str,
        path: Option<&str>,
    ) -> Result<(), StoreError> {
        let plan_id = permit.plan_id();
        let task_id = permit.task_id();
        let outcome_column = report_outcome_to_column(outcome);
        let reported_at = store::current_epoch_secs();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_fixed(
            "UPDATE plan_task SET report_outcome = ?3, report_summary = ?4, \
             report_path = ?5, reported_at = ?6 WHERE plan_id = ?1 AND task_id = ?2",
            params![plan_id, task_id, outcome_column, summary, path, reported_at],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records `state` as `permit`'s task's instruction-notice state: exactly
    /// the two notice columns, nothing else. `at` is stamped here -- `None`
    /// for pending, the current epoch seconds otherwise.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying transaction.
    pub fn write_instruction_notice(
        &mut self,
        permit: &NoticePermit,
        state: yagura_protocol::InstructionNoticeState,
    ) -> Result<(), StoreError> {
        let at = match state {
            yagura_protocol::InstructionNoticeState::Pending => None,
            yagura_protocol::InstructionNoticeState::Delivered
            | yagura_protocol::InstructionNoticeState::WindowClosed => {
                Some(store::current_epoch_secs())
            }
        };
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_fixed(
            "UPDATE plan_task SET instruction_notice_state = ?3, instruction_notice_at = ?4 \
             WHERE plan_id = ?1 AND task_id = ?2",
            params![
                permit.plan_id(),
                permit.task_id(),
                instruction_notice_state_to_column(state),
                at
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Sets `permit`'s plan's owner to `permit`'s nominated window.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying transaction.
    pub fn set_owner(&mut self, permit: NominatePermit) -> Result<(), StoreError> {
        let plan_id = permit.plan_id().to_string();
        let new_owner = permit.new_owner_window().to_string();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_fixed(
            "UPDATE plan SET owner_window = ?2, updated_at = ?3 WHERE plan_id = ?1",
            params![plan_id, new_owner, store::current_epoch_secs()],
        )?;
        tx.commit()?;
        Ok(())
    }
}
