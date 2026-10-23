/**
 * Pure display logic for the plans board: lays out the wave columns and
 * assignment labels the daemon already computed, and derives window liveness
 * from the GUI's own window list rather than from the plan, which carries no
 * liveness, so a card follows a window closing or opening without the plan
 * being read again.
 */
import type {
  AssignmentSource,
  PlanInfo,
  PlanSummary,
  PlanTaskInfo,
  ReportOutcome,
  ResolvedAssignment,
  TaskAssignment,
  ReviewMark,
} from '../ipc/plan-boundary';
import { daemonRejectionCode } from '../ipc/daemon-rejection';
import type { DetectedStatusView } from '../ipc/session-boundary';
import { limitGuidance, profileLimit, type LaunchProfile } from '../profile/launch-profile-choices';
import { plans } from '../ui-messages';
import type { WindowEntry } from '../stores/window-registry';

/** The least of a window-list entry the board needs to read. */
export type BoardWindow = Pick<WindowEntry, 'windowId' | 'name' | 'detected'>;

/** A window a plan names, resolved against the window list. */
export type BoardWindowState =
  { kind: 'live'; windowId: string; name: string; detected: DetectedStatusView } | { kind: 'none' };

/** One task, laid out with the window state of the window it names. */
export interface TaskCard {
  task: PlanTaskInfo;
  window: BoardWindowState;
}

/** One column of the board, in the daemon's own wave order. */
export interface WaveColumn {
  /** The wave's position in the daemon's `waves`. */
  wave: number;
  cards: TaskCard[];
}

/** What changed and may call for the board to read something again. */
export type PlanBoardEvent =
  | { kind: 'plan-changed'; planId: string }
  | { kind: 'roles-saved' }
  | { kind: 'window-list-changed' };

/** What to read again after a {@link PlanBoardEvent}. */
export interface PlanReload {
  /** Whether the plan list is read again. */
  list: boolean;
  /** Whether the role table is read again. */
  roles: boolean;
  /** The plan to read again in full, `null` for none. */
  planId: string | null;
}

/** The assignment editor's raw text fields, before they become a {@link TaskAssignment}. */
export interface EditorFields {
  profile: string;
  model: string;
  effort: string;
}

/** The banner a plan shows about its review, `null` for none. */
export type ReviewBannerKind = 'pending' | 'approved' | 'changed' | null;

/** What the rail flags about the plans, and for how many of them; `null` for nothing. */
export type PlansAttention = { kind: 'pending' | 'changed'; count: number } | null;

/** What made the board read the selected plan in full: the user opening it, or a push. */
export type PlanReadTrigger = 'open' | 'push';

/**
 * The one liveness rule the board uses: a window not in the list, or one
 * the list shows as `exited`, is not live. `isWindowLive` and
 * `resolveBoardWindow` both read it here, so the rule cannot drift between
 * the two, and every place the board shows a window (owner, bound window,
 * candidate list) goes through one of them.
 */
function liveEntry(windows: readonly BoardWindow[], windowId: string | null): BoardWindow | null {
  const entry = windows.find((window) => window.windowId === windowId);
  if (entry === undefined || entry.detected.status === 'exited') {
    return null;
  }
  return entry;
}

/** Whether `windowId` is in the window list and not `exited`. See {@link liveEntry}. */
export function isWindowLive(windows: readonly BoardWindow[], windowId: string | null): boolean {
  return liveEntry(windows, windowId) !== null;
}

/** Resolves a window id the plan names against the window list. See {@link liveEntry}. */
export function resolveBoardWindow(
  windows: readonly BoardWindow[],
  windowId: string | null
): BoardWindowState {
  const entry = liveEntry(windows, windowId);
  return entry === null
    ? { kind: 'none' }
    : { kind: 'live', windowId: entry.windowId, name: entry.name, detected: entry.detected };
}

/** The plan's owner, as the board shows it. */
export function planOwner(plan: PlanInfo, windows: readonly BoardWindow[]): BoardWindowState {
  return resolveBoardWindow(windows, plan.owner_window);
}

/**
 * Lays out the wave columns from `plan.waves` and `plan.tasks` alone: the
 * daemon has already computed the grouping and the order, so this only reads
 * it and resolves each task's bound window against the window list.
 */
export function buildWaveColumns(plan: PlanInfo, windows: readonly BoardWindow[]): WaveColumn[] {
  const taskById = new Map(plan.tasks.map((task) => [task.task_id, task]));
  return plan.waves.map((taskIds, wave) => ({
    wave,
    cards: taskIds.flatMap((taskId) => {
      const task = taskById.get(taskId);
      if (task === undefined) {
        return [];
      }
      return [{ task, window: resolveBoardWindow(windows, task.window_id) }];
    }),
  }));
}

const ASSIGNMENT_SOURCE_KEYS: Record<AssignmentSource, string> = {
  user: 'sourceUser',
  parent: 'sourceParent',
  role: 'sourceRole',
  default: 'sourceDefault',
  unassigned: 'sourceUnassigned',
};

/** The plans message key that labels one {@link AssignmentSource}. */
export function assignmentSourceKey(source: AssignmentSource): string {
  return ASSIGNMENT_SOURCE_KEYS[source];
}

const REPORT_OUTCOME_KEYS: Record<ReportOutcome, string> = {
  succeeded: 'reportSucceeded',
  failed: 'reportFailed',
};

/** The plans message key that labels one {@link ReportOutcome}. */
export function reportOutcomeKey(outcome: ReportOutcome): string {
  return REPORT_OUTCOME_KEYS[outcome];
}

/** Whether a resolved assignment has no winning source. */
export function isUnassigned(resolved: ResolvedAssignment): boolean {
  return resolved.source === 'unassigned';
}

/**
 * What to read again for one board event. A `plan-changed` push for a plan
 * other than the one shown only means the summary list is stale; a push for
 * the shown plan itself is read in full, and the list as well since the
 * selector shows its title from the list. Only the window list changing
 * reads nothing, since liveness comes from the window list directly.
 */
export function planReloadFor(event: PlanBoardEvent, selectedPlanId: string | null): PlanReload {
  switch (event.kind) {
    case 'plan-changed':
      return event.planId === selectedPlanId
        ? { list: true, roles: false, planId: selectedPlanId }
        : { list: true, roles: false, planId: null };
    case 'roles-saved':
      return { list: false, roles: true, planId: selectedPlanId };
    case 'window-list-changed':
      return { list: false, roles: false, planId: null };
  }
}

/**
 * Drops the selection only when the read that failed named the plan still
 * selected and was refused as `plan_not_found`; a selection made after the
 * failed read was sent no longer matches `failedPlanId` and is kept.
 */
export function selectionAfterPlanReadFailure(
  selectedPlanId: string | null,
  failedPlanId: string,
  error: unknown
): string | null {
  if (selectedPlanId !== failedPlanId) {
    return selectedPlanId;
  }
  return daemonRejectionCode(error) === 'plan_not_found' ? null : selectedPlanId;
}

/** Builds the assignment the editor's fields describe; empty text means "not set". */
export function assignmentFromEditor(fields: EditorFields): TaskAssignment {
  return {
    profile: fields.profile,
    model: fields.model === '' ? null : fields.model,
    effort: fields.effort === '' ? null : fields.effort,
  };
}

/**
 * Windows offered for the owner picker, in window-list order. Built from the
 * window list alone (not from the plan's own window ids) so a window that
 * opens after the plan was read is offered too, via {@link isWindowLive}.
 * `plan` is unused: the candidate list intentionally is not narrowed to
 * windows the plan already names.
 */
export function ownerCandidates(
  _plan: PlanInfo,
  windows: readonly BoardWindow[]
): { windowId: string; name: string }[] {
  return windows
    .filter((window) => isWindowLive(windows, window.windowId))
    .map((window) => ({ windowId: window.windowId, name: window.name }));
}

/**
 * The one line saying why the daemon will refuse to open this task's window,
 * `null` when it will not. Only a task with an instruction, assigned a profile
 * that cannot take it at launch, is refused; the rest is left to the daemon.
 */
export function taskOpenRefusal(
  task: PlanTaskInfo,
  profiles: readonly Pick<LaunchProfile, 'id' | 'displayName' | 'limits'>[]
): string | null {
  const profileId = task.has_instruction ? task.resolved.assignment?.profile : undefined;
  const profile = profiles.find((entry) => entry.id === profileId);
  const limit = profileId === undefined ? null : profileLimit(profiles, profileId, 'launch_prompt');
  if (profile === undefined || limit === null) {
    return null;
  }
  const guidance = limitGuidance(limit)?.text ?? null;
  return plans.taskOpenLimited(
    profile.displayName,
    guidance === null ? null : guidance.replace(/\s+/g, ' ').trim()
  );
}

/** The editor's text fields for an assignment; an unset value reads as blank text. */
export function editorFieldsOf(assignment: TaskAssignment | null): EditorFields {
  return {
    profile: assignment?.profile ?? '',
    model: assignment?.model ?? '',
    effort: assignment?.effort ?? '',
  };
}

function hasReviewMark(plan: PlanInfo): boolean {
  return plan.tasks.some((task) => task.review_mark !== null);
}

/** The review banner for `plan`: an approved plan whose tasks carry marks shows as changed. */
export function reviewBannerKind(plan: PlanInfo): ReviewBannerKind {
  switch (plan.review) {
    case null:
      return null;
    case 'pending':
      return 'pending';
    case 'approved':
      return hasReviewMark(plan) ? 'changed' : 'approved';
  }
}

/**
 * The tasks, in plan order, whose editor text differs from the assignment they
 * show. Compared by value, so an edit typed back to the shown values, or one
 * discarded, no longer counts.
 */
export function unappliedTaskIds(
  plan: PlanInfo,
  drafts: Readonly<Record<string, EditorFields>>
): readonly string[] {
  return plan.tasks
    .filter((task) => {
      // A task id is daemon text: an inherited key such as `constructor` is no draft.
      const draft = Object.prototype.hasOwnProperty.call(drafts, task.task_id)
        ? drafts[task.task_id]
        : undefined;
      if (draft === undefined) {
        return false;
      }
      const shown = editorFieldsOf(task.resolved.assignment);
      return (
        draft.profile !== shown.profile ||
        draft.model !== shown.model ||
        draft.effort !== shown.effort
      );
    })
    .map((task) => task.task_id);
}

/**
 * What the rail flags about the plans. Plans waiting for approval come first,
 * since nothing runs from them until the user acts; unseen changes otherwise.
 */
export function plansAttention(summaries: readonly PlanSummary[]): PlansAttention {
  const pending = summaries.filter((summary) => summary.review === 'pending').length;
  if (pending > 0) {
    return { kind: 'pending', count: pending };
  }
  const changed = summaries.filter((summary) => summary.unseen_changes).length;
  return changed > 0 ? { kind: 'changed', count: changed } : null;
}

/**
 * Whether reading `plan` should tell the daemon the user has seen it. Only
 * the user opening a plan counts; a read caused by a push is not a look.
 */
export function shouldMarkPlanSeen(trigger: PlanReadTrigger, plan: PlanInfo): boolean {
  return trigger === 'open' && hasReviewMark(plan);
}

/** The mark each marked task of `plan` carries, keyed by task id. */
export function reviewMarksOf(plan: PlanInfo): Readonly<Record<string, ReviewMark>> {
  const marks: Record<string, ReviewMark> = {};
  for (const task of plan.tasks) {
    if (task.review_mark !== null) {
      marks[task.task_id] = task.review_mark;
    }
  }
  return marks;
}

/**
 * `plan` with the marks the user was shown kept on its tasks. Telling the
 * daemon a plan was seen clears its marks at once, so without this a plan
 * opened after it changed would hide what changed from the one look it gets.
 */
export function withRetainedMarks(
  plan: PlanInfo,
  retained: Readonly<Record<string, ReviewMark>>
): PlanInfo {
  if (Object.keys(retained).length === 0) {
    return plan;
  }
  return {
    ...plan,
    tasks: plan.tasks.map((task) => {
      const kept = Object.prototype.hasOwnProperty.call(retained, task.task_id)
        ? retained[task.task_id]
        : undefined;
      return task.review_mark === null && kept !== undefined
        ? { ...task, review_mark: kept }
        : task;
    }),
  };
}
