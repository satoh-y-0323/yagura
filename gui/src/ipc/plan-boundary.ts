/**
 * The WebView-side boundary for the plan ledger: the shapes the plan
 * commands reply with and the `session://plan-changed` payload.
 *
 * The field names mirror the Rust wire types in `yagura-protocol` verbatim
 * (snake_case). Every reply is accepted as `unknown` and validated here with
 * the same readers `session-boundary` uses, so a field is only ever read
 * through an ownership test.
 */
import {
  BoundaryError,
  readOptionalString,
  readOwnField,
  requireArray,
  requireNonNegativeInteger,
  requireRecord,
  requireString,
  requireStringArray,
  requireStringList,
  SESSION_EVENT,
  type UntrustedRecord,
} from './session-boundary';

export { BoundaryError };

/**
 * Where a task's resolved assignment came from. A closed set, like the Rust
 * enum it mirrors: a value outside it is refused rather than normalized.
 */
export type AssignmentSource = 'user' | 'parent' | 'role' | 'default' | 'unassigned';

const ASSIGNMENT_SOURCE_VALUES: ReadonlySet<string> = new Set<AssignmentSource>([
  'user',
  'parent',
  'role',
  'default',
  'unassigned',
]);

/** Where a plan stands in its review: waiting for the user, or approved. */
export type PlanReview = 'pending' | 'approved';

const PLAN_REVIEW_VALUES: ReadonlySet<string> = new Set<PlanReview>(['pending', 'approved']);

/** Why a task of an approved plan asks for another look: added, or edited. */
export type ReviewMark = 'new' | 'changed';

const REVIEW_MARK_VALUES: ReadonlySet<string> = new Set<ReviewMark>(['new', 'changed']);

/**
 * Whether a reported piece of work succeeded. A closed set, like the Rust
 * enum it mirrors: a value outside it is refused rather than normalized.
 */
export type ReportOutcome = 'succeeded' | 'failed';

const REPORT_OUTCOME_VALUES: ReadonlySet<string> = new Set<ReportOutcome>(['succeeded', 'failed']);

/** The latest report recorded against a task. */
export interface TaskReport {
  outcome: ReportOutcome;
  /** The reporting window's account, untrusted text shown only as text. */
  summary: string;
  /** Where a longer result was written, `null` when none was named. */
  path: string | null;
  /** When the report was recorded, in Unix epoch seconds. */
  reported_at: number;
}

/** A launch profile together with the launch options passed through it. */
export interface TaskAssignment {
  /** The launch profile a task's window opens from. */
  profile: string;
  /** The `model` launch option, `null` when none is given. */
  model: string | null;
  /** The `effort` launch option, `null` when none is given. */
  effort: string | null;
}

/** A task's effective assignment and the source it was taken from. */
export interface ResolvedAssignment {
  /** Which source won. */
  source: AssignmentSource;
  /** The winning assignment, `null` when no source supplied one. */
  assignment: TaskAssignment | null;
}

/** One task of a stored plan. */
export interface PlanTaskInfo {
  task_id: string;
  title: string;
  /** The role the task names, `null` when it names none. */
  role: string | null;
  depends_on: string[];
  writes: string[];
  /** The index of the wave the task belongs to, counted from zero. */
  wave: number;
  /** The planning window's proposal, `null` when it made none. */
  parent: TaskAssignment | null;
  /** The user's override, `null` when none is set. */
  user: TaskAssignment | null;
  resolved: ResolvedAssignment;
  /** The window last opened for the task, kept even after it closes. */
  window_id: string | null;
  /** The latest report recorded against the task, `null` when there is none. */
  report: TaskReport | null;
  /** Why the task needs another look since the last approval, `null` when it does not. */
  review_mark: ReviewMark | null;
  /** Whether the task carries an instruction, which its window is handed at launch. */
  has_instruction: boolean;
}

/** One stored plan in full, as `get_plan` and every plan update reply with it. */
export interface PlanInfo {
  plan_id: string;
  title: string;
  cwd: string;
  /** The owning window, kept even after it closes; `null` when unowned. */
  owner_window: string | null;
  /** The task identifiers grouped by wave, in wave order. */
  waves: string[][];
  tasks: PlanTaskInfo[];
  /** Why the role table took no part in resolving assignments, if it did not. */
  roles_warning: string | null;
  /** The plan's review state, `null` when the plan is under no review. */
  review: PlanReview | null;
  /** The stored revision, echoed back when approving or marking the plan seen. */
  version: number;
}

/** One row of the `list_plans` result. */
export interface PlanSummary {
  plan_id: string;
  title: string;
  /** The owning window, kept even after it closes; `null` when unowned. */
  owner_window: string | null;
  task_count: number;
  /** The plan's review state, `null` when the plan is under no review. */
  review: PlanReview | null;
  /** Whether an approved plan has changes the user has not looked at yet. */
  unseen_changes: boolean;
}

/** One row of the role-to-assignment table. */
export interface RoleEntry {
  /** The role's name, as tasks refer to it. */
  name: string;
  profile: string;
  model: string | null;
  effort: string | null;
}

/** The role-to-assignment table together with the role used when a task names none. */
export interface RoleTable {
  roles: RoleEntry[];
  /** The role a task without a role of its own falls back to, `null` when none is set. */
  default_role: string | null;
}

/** Payload of `session://plan-changed`. */
export interface PlanChanged {
  /** The plan whose stored content changed. */
  plan_id: string;
}

/**
 * Validates a reply carrying one whole plan.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
export function narrowPlanInfo(value: unknown): PlanInfo {
  const context = 'plan';
  const record = requireRecord(value, context);
  const wavesContext = `${context}.waves`;
  const tasksContext = `${context}.tasks`;
  return {
    plan_id: requireString(record, 'plan_id', context),
    title: requireString(record, 'title', context),
    cwd: requireString(record, 'cwd', context),
    owner_window: readOptionalString(record, 'owner_window', context),
    waves: requireArray(readOwnField(record, 'waves'), wavesContext).map((wave) =>
      requireStringList(wave, wavesContext)
    ),
    tasks: requireArray(readOwnField(record, 'tasks'), tasksContext).map((task) =>
      narrowPlanTaskInfo(task, tasksContext)
    ),
    roles_warning: readOptionalString(record, 'roles_warning', context),
    review: readOptionalClosed<PlanReview>(record, 'review', context, PLAN_REVIEW_VALUES),
    version: readVersion(record, context),
  };
}

/**
 * Validates the reply to `list_plans`: the envelope `{ plans: [...] }` the
 * daemon's own `PlanList` message carries, not a bare array.
 *
 * @throws {BoundaryError} when the reply, or any row in it, does not match
 * the contract.
 */
export function narrowPlanList(value: unknown): PlanSummary[] {
  const context = 'list_plans';
  const record = requireRecord(value, context);
  const plansContext = `${context}.plans`;
  return requireArray(readOwnField(record, 'plans'), plansContext).map((entry) => {
    const row = requireRecord(entry, plansContext);
    return {
      plan_id: requireString(row, 'plan_id', plansContext),
      title: requireString(row, 'title', plansContext),
      owner_window: readOptionalString(row, 'owner_window', plansContext),
      task_count: requireNonNegativeInteger(row, 'task_count', plansContext),
      review: readOptionalClosed<PlanReview>(row, 'review', plansContext, PLAN_REVIEW_VALUES),
      unseen_changes: readFlag(row, 'unseen_changes', plansContext),
    };
  });
}

/**
 * Validates the reply to `list_roles` and `save_roles`: the envelope
 * `{ roles: [...], default_role }` the daemon's own `RoleList` message carries.
 *
 * @throws {BoundaryError} when the reply, or any row in it, does not match
 * the contract.
 */
export function narrowRoleList(value: unknown): RoleTable {
  const context = 'list_roles';
  const record = requireRecord(value, context);
  const rolesContext = `${context}.roles`;
  const roles = requireArray(readOwnField(record, 'roles'), rolesContext).map((entry) => {
    const row = requireRecord(entry, rolesContext);
    return {
      name: requireString(row, 'name', rolesContext),
      profile: requireString(row, 'profile', rolesContext),
      model: readOptionalString(row, 'model', rolesContext),
      effort: readOptionalString(row, 'effort', rolesContext),
    };
  });
  return { roles, default_role: readOptionalString(record, 'default_role', context) };
}

/**
 * Validates a `session://plan-changed` payload.
 *
 * @throws {BoundaryError} when the payload does not match the contract.
 */
export function narrowPlanChanged(value: unknown): PlanChanged {
  const context = SESSION_EVENT.planChanged;
  const record = requireRecord(value, context);
  return { plan_id: requireString(record, 'plan_id', context) };
}

function narrowPlanTaskInfo(value: unknown, context: string): PlanTaskInfo {
  const record = requireRecord(value, context);
  return {
    task_id: requireString(record, 'task_id', context),
    title: requireString(record, 'title', context),
    role: readOptionalString(record, 'role', context),
    depends_on: requireStringArray(record, 'depends_on', context),
    writes: requireStringArray(record, 'writes', context),
    wave: requireNonNegativeInteger(record, 'wave', context),
    parent: readOptionalAssignment(record, 'parent', context),
    user: readOptionalAssignment(record, 'user', context),
    resolved: narrowResolvedAssignment(readOwnField(record, 'resolved'), `${context}.resolved`),
    window_id: readOptionalString(record, 'window_id', context),
    report: readOptionalReport(record, context),
    review_mark: readOptionalClosed<ReviewMark>(record, 'review_mark', context, REVIEW_MARK_VALUES),
    has_instruction: readOptionalString(record, 'instruction', context) !== null,
  };
}

function narrowResolvedAssignment(value: unknown, context: string): ResolvedAssignment {
  const record = requireRecord(value, context);
  return {
    source: requireAssignmentSource(record, context),
    assignment: readOptionalAssignment(record, 'assignment', context),
  };
}

function requireAssignmentSource(record: UntrustedRecord, context: string): AssignmentSource {
  const value = requireString(record, 'source', context);
  if (!ASSIGNMENT_SOURCE_VALUES.has(value)) {
    throw new BoundaryError(`${context}.source: unrecognized source "${value}"`);
  }
  return value as AssignmentSource;
}

/** Reads a task's report; the core omits the key when the task has none. */
function readOptionalReport(record: UntrustedRecord, context: string): TaskReport | null {
  const value = readOwnField(record, 'report');
  if (value === null || value === undefined) {
    return null;
  }
  const reportContext = `${context}.report`;
  const report = requireRecord(value, reportContext);
  return {
    outcome: requireReportOutcome(report, reportContext),
    summary: requireString(report, 'summary', reportContext),
    path: readOptionalString(report, 'path', reportContext),
    reported_at: requireNonNegativeInteger(report, 'reported_at', reportContext),
  };
}

function requireReportOutcome(record: UntrustedRecord, context: string): ReportOutcome {
  const value = requireString(record, 'outcome', context);
  if (!REPORT_OUTCOME_VALUES.has(value)) {
    throw new BoundaryError(`${context}.outcome: unrecognized outcome "${value}"`);
  }
  return value as ReportOutcome;
}

/** Reads an assignment field the core serializes as `null` when it has none. */
function readOptionalAssignment(
  record: UntrustedRecord,
  key: string,
  context: string
): TaskAssignment | null {
  const value = readOwnField(record, key);
  if (value === null || value === undefined) {
    return null;
  }
  const fieldContext = `${context}.${key}`;
  const assignment = requireRecord(value, fieldContext);
  return {
    profile: requireString(assignment, 'profile', fieldContext),
    model: readOptionalString(assignment, 'model', fieldContext),
    effort: readOptionalString(assignment, 'effort', fieldContext),
  };
}

/**
 * Reads an optional member of a closed string set. The core omits the key or
 * sends `null` when there is none; any other value outside the set is refused.
 */
function readOptionalClosed<T extends string>(
  record: UntrustedRecord,
  key: string,
  context: string,
  allowed: ReadonlySet<string>
): T | null {
  const value = readOptionalString(record, key, context);
  if (value !== null && !allowed.has(value)) {
    throw new BoundaryError(`${context}.${key}: unrecognized value "${value}"`);
  }
  return value as T | null;
}

/** Reads the plan version; a core that predates versions omits it, read as zero. */
function readVersion(record: UntrustedRecord, context: string): number {
  return readOwnField(record, 'version') === undefined
    ? 0
    : requireNonNegativeInteger(record, 'version', context);
}

/**
 * Reads a flag the core omits when it predates it, read as `false`. A present
 * `null` is refused, as the Rust `bool` it mirrors does not decode one.
 */
function readFlag(record: UntrustedRecord, key: string, context: string): boolean {
  const value = readOwnField(record, key);
  if (value === undefined) {
    return false;
  }
  if (typeof value !== 'boolean') {
    throw new BoundaryError(`${context}.${key}: expected a boolean`);
  }
  return value;
}
