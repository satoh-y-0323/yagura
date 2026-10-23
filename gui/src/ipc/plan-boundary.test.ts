/**
 * TS-boundary contract for the plan ledger: the narrowing functions that turn
 * a daemon-shaped reply into a typed `PlanInfo` / `PlanSummary[]` /
 * `RoleTable` / `PlanChanged`, plus the closed `AssignmentSource`
 * vocabulary those replies carry (`user` / `parent` / `role` / `default` /
 * `unassigned`, no catch-all -- mirroring the Rust enum of the same name,
 * which decodes nothing outside that set), and the closed `ReportOutcome` a task's report
 * carries (`succeeded` / `failed`), refused rather than normalized likewise.
 *
 * Lives in its own file, next to `plan-boundary.ts`, the way
 * `session-client.chat.test.ts` sits next to the chat bridge it covers.
 */
import { describe, it, expect } from 'vitest';

import {
  BoundaryError,
  narrowPlanChanged,
  narrowPlanInfo,
  narrowPlanList,
  narrowRoleList,
  type AssignmentSource,
  type PlanChanged,
  type PlanInfo,
  type PlanReview,
  type PlanSummary,
  type ReportOutcome,
  type ReviewMark,
  type RoleEntry,
  type RoleTable,
  type TaskReport,
} from './plan-boundary';

/** A well-formed `TaskAssignment` wire value. */
function taskAssignmentWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    profile: 'claude-code',
    model: null,
    effort: null,
    ...overrides,
  };
}

/** A well-formed `ResolvedAssignment` wire value. */
function resolvedAssignmentWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    source: 'role',
    assignment: taskAssignmentWire(),
    ...overrides,
  };
}

/** A well-formed `PlanTaskInfo` wire value. */
function planTaskInfoWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    task_id: 'investigate',
    title: 'Investigate',
    role: 'review',
    depends_on: [],
    writes: ['gui/src/App.svelte'],
    wave: 0,
    parent: taskAssignmentWire(),
    user: null,
    resolved: resolvedAssignmentWire(),
    window_id: 'window-a',
    ...overrides,
  };
}

/** A well-formed `TaskReport` wire value. */
function taskReportWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    outcome: 'succeeded',
    summary: 'Tests pass; the ledger records the report.',
    path: 'reports/impl.md',
    reported_at: 1_790_000_000,
    ...overrides,
  };
}

/** The narrowed report of the first task of a plan whose only task carries `report`. */
function narrowedReport(report: unknown): TaskReport | null | undefined {
  const info = narrowPlanInfo(planInfoWire({ tasks: [planTaskInfoWire({ report })] }));
  return info.tasks[0]?.report;
}

/** Whether a plan whose only task carries `report` is refused at the boundary. */
function expectReportRejected(report: unknown): void {
  expect(() => narrowPlanInfo(planInfoWire({ tasks: [planTaskInfoWire({ report })] }))).toThrow(
    BoundaryError
  );
}

/** A well-formed `PlanInfo` wire value, as `get_plan` (and friends) reply with it. */
function planInfoWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    plan_id: 'p-abc123',
    title: 'Plan window launcher',
    cwd: 'C:\\repo',
    owner_window: 'window-a',
    waves: [['investigate']],
    tasks: [planTaskInfoWire()],
    roles_warning: null,
    ...overrides,
  };
}

/** A well-formed `PlanSummary` wire row. */
function planSummaryWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    plan_id: 'p-abc123',
    title: 'Plan window launcher',
    owner_window: 'window-a',
    task_count: 1,
    ...overrides,
  };
}

/** A well-formed `RoleEntry` wire row. */
function roleEntryWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    name: 'review',
    profile: 'codex-cli',
    model: null,
    effort: null,
    ...overrides,
  };
}

describe('narrowPlanInfo', () => {
  it('accepts a well-formed plan', () => {
    const info: PlanInfo = narrowPlanInfo(planInfoWire());
    expect(info.plan_id).toBe('p-abc123');
    expect(info.waves).toEqual([['investigate']]);
    expect(info.tasks).toHaveLength(1);
    expect(info.tasks[0]?.resolved.source).toBe('role');
  });

  it('accepts a null owner_window', () => {
    const info = narrowPlanInfo(planInfoWire({ owner_window: null }));
    expect(info.owner_window).toBeNull();
  });

  it('accepts a null roles_warning', () => {
    const info = narrowPlanInfo(planInfoWire({ roles_warning: null }));
    expect(info.roles_warning).toBeNull();
  });

  it('accepts a task whose parent and user assignments are both null', () => {
    const info = narrowPlanInfo(
      planInfoWire({ tasks: [planTaskInfoWire({ parent: null, user: null })] })
    );
    expect(info.tasks[0]?.parent).toBeNull();
    expect(info.tasks[0]?.user).toBeNull();
  });

  it('accepts a task whose window_id is null', () => {
    const info = narrowPlanInfo(planInfoWire({ tasks: [planTaskInfoWire({ window_id: null })] }));
    expect(info.tasks[0]?.window_id).toBeNull();
  });

  it('reads whether a task carries an instruction, which the core omits when it has none', () => {
    const withOne = narrowPlanInfo(
      planInfoWire({ tasks: [planTaskInfoWire({ instruction: 'Say hello.' })] })
    );
    const without = narrowPlanInfo(planInfoWire({ tasks: [planTaskInfoWire()] }));
    expect(withOne.tasks[0]?.has_instruction).toBe(true);
    expect(without.tasks[0]?.has_instruction).toBe(false);
  });

  it('accepts a resolved assignment with no source and a null assignment', () => {
    const info = narrowPlanInfo(
      planInfoWire({
        tasks: [
          planTaskInfoWire({
            resolved: resolvedAssignmentWire({ source: 'unassigned', assignment: null }),
          }),
        ],
      })
    );
    expect(info.tasks[0]?.resolved).toEqual({ source: 'unassigned', assignment: null });
  });

  it('rejects a non-object value', () => {
    expect(() => narrowPlanInfo(null)).toThrow(BoundaryError);
    expect(() => narrowPlanInfo('nope')).toThrow(BoundaryError);
  });

  it('rejects missing plan_id', () => {
    const wire = planInfoWire();
    delete wire['plan_id'];
    expect(() => narrowPlanInfo(wire)).toThrow(BoundaryError);
  });

  it('rejects missing cwd', () => {
    const wire = planInfoWire();
    delete wire['cwd'];
    expect(() => narrowPlanInfo(wire)).toThrow(BoundaryError);
  });

  it('rejects waves that is not an array', () => {
    expect(() => narrowPlanInfo(planInfoWire({ waves: 'investigate' }))).toThrow(BoundaryError);
  });

  it('rejects a wave entry that is not an array of strings', () => {
    expect(() => narrowPlanInfo(planInfoWire({ waves: [['investigate', 1]] }))).toThrow(
      BoundaryError
    );
  });

  it('rejects tasks that is not an array', () => {
    expect(() => narrowPlanInfo(planInfoWire({ tasks: 'nope' }))).toThrow(BoundaryError);
  });

  it('rejects a task missing task_id', () => {
    const task = planTaskInfoWire();
    delete task['task_id'];
    expect(() => narrowPlanInfo(planInfoWire({ tasks: [task] }))).toThrow(BoundaryError);
  });

  it('rejects a task whose depends_on is not an array', () => {
    expect(() =>
      narrowPlanInfo(planInfoWire({ tasks: [planTaskInfoWire({ depends_on: 'a' })] }))
    ).toThrow(BoundaryError);
  });

  it('rejects a task whose writes entry is not a string', () => {
    expect(() =>
      narrowPlanInfo(planInfoWire({ tasks: [planTaskInfoWire({ writes: [1] })] }))
    ).toThrow(BoundaryError);
  });

  it('rejects a task whose wave is not an integer', () => {
    expect(() =>
      narrowPlanInfo(planInfoWire({ tasks: [planTaskInfoWire({ wave: 'zero' })] }))
    ).toThrow(BoundaryError);
  });

  it('rejects a task whose wave is negative', () => {
    expect(() => narrowPlanInfo(planInfoWire({ tasks: [planTaskInfoWire({ wave: -1 })] }))).toThrow(
      BoundaryError
    );
  });

  it('rejects a task assignment missing profile', () => {
    const assignment = taskAssignmentWire();
    delete assignment['profile'];
    expect(() =>
      narrowPlanInfo(planInfoWire({ tasks: [planTaskInfoWire({ parent: assignment })] }))
    ).toThrow(BoundaryError);
  });

  it('rejects a task assignment whose model is not a string or null', () => {
    expect(() =>
      narrowPlanInfo(
        planInfoWire({ tasks: [planTaskInfoWire({ parent: taskAssignmentWire({ model: 7 }) })] })
      )
    ).toThrow(BoundaryError);
  });

  it('rejects a resolved assignment with an unrecognized source', () => {
    expect(() =>
      narrowPlanInfo(
        planInfoWire({
          tasks: [
            planTaskInfoWire({
              resolved: resolvedAssignmentWire({ source: 'a_future_source' }),
            }),
          ],
        })
      )
    ).toThrow(BoundaryError);
  });

  it('rejects a resolved assignment missing source', () => {
    const resolved = resolvedAssignmentWire();
    delete resolved['source'];
    expect(() => narrowPlanInfo(planInfoWire({ tasks: [planTaskInfoWire({ resolved })] }))).toThrow(
      BoundaryError
    );
  });

  it('rejects a task whose window_id is a non-string, non-null value', () => {
    expect(() =>
      narrowPlanInfo(planInfoWire({ tasks: [planTaskInfoWire({ window_id: 5 })] }))
    ).toThrow(BoundaryError);
  });
});

describe('AssignmentSource', () => {
  it('accepts each of the four known sources, with a null assignment for unassigned', () => {
    const sources: AssignmentSource[] = ['user', 'parent', 'role', 'unassigned'];
    for (const source of sources) {
      const info = narrowPlanInfo(
        planInfoWire({
          tasks: [
            planTaskInfoWire({
              resolved: resolvedAssignmentWire({
                source,
                assignment: source === 'unassigned' ? null : taskAssignmentWire(),
              }),
            }),
          ],
        })
      );
      expect(info.tasks[0]?.resolved.source).toBe(source);
    }
  });
});

describe('TaskReport', () => {
  it('reads a task without a report key as having no report', () => {
    const info = narrowPlanInfo(planInfoWire());
    expect(info.tasks[0]?.report).toBeNull();
  });

  it('reads a task whose report is null as having no report', () => {
    expect(narrowedReport(null)).toBeNull();
  });

  it('keeps every field of a well-formed report as the wire carried it', () => {
    const expected: TaskReport = {
      outcome: 'failed',
      summary: 'Two cases still fail on Linux.',
      path: 'reports/confirm.md',
      reported_at: 1_790_000_123,
    };
    expect(narrowedReport(taskReportWire({ ...expected }))).toEqual(expected);
  });

  it('accepts a report whose path is null', () => {
    expect(narrowedReport(taskReportWire({ path: null }))).toEqual({
      outcome: 'succeeded',
      summary: 'Tests pass; the ledger records the report.',
      path: null,
      reported_at: 1_790_000_000,
    });
  });

  it('accepts an empty summary and a reported_at of zero', () => {
    const report = narrowedReport(taskReportWire({ summary: '', reported_at: 0 }));
    expect(report?.summary).toBe('');
    expect(report?.reported_at).toBe(0);
  });

  it('accepts each of the two known outcomes', () => {
    const outcomes: ReportOutcome[] = ['succeeded', 'failed'];
    for (const outcome of outcomes) {
      expect(narrowedReport(taskReportWire({ outcome }))?.outcome).toBe(outcome);
    }
  });

  it('rejects an outcome outside the closed set rather than normalizing it', () => {
    for (const outcome of ['done', 'Succeeded', 'FAILED', '']) {
      expectReportRejected(taskReportWire({ outcome }));
    }
  });

  it('rejects an outcome that is not a string', () => {
    expectReportRejected(taskReportWire({ outcome: 1 }));
    expectReportRejected(taskReportWire({ outcome: null }));
  });

  it('rejects a report missing its outcome', () => {
    const report = taskReportWire();
    delete report['outcome'];
    expectReportRejected(report);
  });

  it('rejects a summary that is not a string', () => {
    expectReportRejected(taskReportWire({ summary: 42 }));
    expectReportRejected(taskReportWire({ summary: null }));
  });

  it('rejects a report missing its summary', () => {
    const report = taskReportWire();
    delete report['summary'];
    expectReportRejected(report);
  });

  it('rejects a path that is neither a string nor null', () => {
    expectReportRejected(taskReportWire({ path: 7 }));
    expectReportRejected(taskReportWire({ path: ['reports/impl.md'] }));
  });

  it('rejects a reported_at that is not a non-negative integer', () => {
    for (const reportedAt of [-1, 1.5, '1790000000', null]) {
      expectReportRejected(taskReportWire({ reported_at: reportedAt }));
    }
  });

  it('rejects a report missing its reported_at', () => {
    const report = taskReportWire();
    delete report['reported_at'];
    expectReportRejected(report);
  });

  it('rejects a report that is not an object', () => {
    expectReportRejected('succeeded');
    expectReportRejected(['succeeded']);
  });
});

describe('narrowPlanList', () => {
  it('accepts a well-formed reply, keeping the rows in the order they arrived', () => {
    const plans: PlanSummary[] = narrowPlanList({
      plans: [planSummaryWire(), planSummaryWire({ plan_id: 'p-def456', owner_window: null })],
    });
    expect(plans).toHaveLength(2);
    expect(plans[0]?.plan_id).toBe('p-abc123');
    expect(plans[1]?.owner_window).toBeNull();
  });

  it('accepts an empty plan list', () => {
    expect(narrowPlanList({ plans: [] })).toEqual([]);
  });

  it('rejects a non-object value', () => {
    expect(() => narrowPlanList(null)).toThrow(BoundaryError);
  });

  it('rejects a bare array in place of the envelope', () => {
    expect(() => narrowPlanList([planSummaryWire()])).toThrow(BoundaryError);
  });

  it('rejects a missing plans field', () => {
    expect(() => narrowPlanList({})).toThrow(BoundaryError);
  });

  it('rejects plans that is not an array', () => {
    expect(() => narrowPlanList({ plans: 'nope' })).toThrow(BoundaryError);
  });

  it('rejects a row missing plan_id', () => {
    const wire = planSummaryWire();
    delete wire['plan_id'];
    expect(() => narrowPlanList({ plans: [wire] })).toThrow(BoundaryError);
  });

  it('rejects a row missing task_count', () => {
    const wire = planSummaryWire();
    delete wire['task_count'];
    expect(() => narrowPlanList({ plans: [wire] })).toThrow(BoundaryError);
  });

  it('rejects a negative task_count', () => {
    expect(() => narrowPlanList({ plans: [planSummaryWire({ task_count: -1 })] })).toThrow(
      BoundaryError
    );
  });

  it('rejects a fractional task_count', () => {
    expect(() => narrowPlanList({ plans: [planSummaryWire({ task_count: 1.5 })] })).toThrow(
      BoundaryError
    );
  });
});

describe('narrowRoleList', () => {
  it('accepts a well-formed reply, keeping the rows in the order they arrived', () => {
    const roles: RoleEntry[] = narrowRoleList({
      roles: [roleEntryWire(), roleEntryWire({ name: 'implement', model: 'opus' })],
    }).roles;
    expect(roles).toHaveLength(2);
    expect(roles[1]?.name).toBe('implement');
    expect(roles[1]?.model).toBe('opus');
  });

  it('accepts an empty role list', () => {
    expect(narrowRoleList({ roles: [] }).roles).toEqual([]);
  });

  it('rejects a non-object value', () => {
    expect(() => narrowRoleList(null)).toThrow(BoundaryError);
  });

  it('rejects a missing roles field', () => {
    expect(() => narrowRoleList({})).toThrow(BoundaryError);
  });

  it('rejects a row missing name', () => {
    const wire = roleEntryWire();
    delete wire['name'];
    expect(() => narrowRoleList({ roles: [wire] })).toThrow(BoundaryError);
  });

  it('rejects a row missing profile', () => {
    const wire = roleEntryWire();
    delete wire['profile'];
    expect(() => narrowRoleList({ roles: [wire] })).toThrow(BoundaryError);
  });

  it('rejects a row whose name is not a string', () => {
    expect(() => narrowRoleList({ roles: [roleEntryWire({ name: 7 })] })).toThrow(BoundaryError);
  });

  it('rejects a row whose effort is not a string or null', () => {
    expect(() => narrowRoleList({ roles: [roleEntryWire({ effort: 3 })] })).toThrow(BoundaryError);
  });

  it('accepts a row whose model and effort are both null', () => {
    const [role] = narrowRoleList({
      roles: [roleEntryWire({ model: null, effort: null })],
    }).roles;
    expect(role?.model).toBeNull();
    expect(role?.effort).toBeNull();
  });
});

describe('narrowPlanChanged', () => {
  it('accepts a well-formed payload', () => {
    const changed: PlanChanged = narrowPlanChanged({ plan_id: 'p-abc123' });
    expect(changed).toEqual({ plan_id: 'p-abc123' });
  });

  it('rejects a non-object value', () => {
    expect(() => narrowPlanChanged(null)).toThrow(BoundaryError);
    expect(() => narrowPlanChanged('nope')).toThrow(BoundaryError);
  });

  it('rejects missing plan_id', () => {
    expect(() => narrowPlanChanged({})).toThrow(BoundaryError);
  });

  it('rejects a non-string plan_id', () => {
    expect(() => narrowPlanChanged({ plan_id: 5 })).toThrow(BoundaryError);
  });
});

describe('the review state of a plan', () => {
  it('keeps each known review value and the version the daemon reported', () => {
    const reviews: PlanReview[] = ['pending', 'approved'];
    for (const review of reviews) {
      const info = narrowPlanInfo(planInfoWire({ review, version: 7 }));
      expect(info.review).toBe(review);
      expect(info.version).toBe(7);
    }
  });

  it('reads a plan without a review key as under no review', () => {
    expect(narrowPlanInfo(planInfoWire()).review).toBeNull();
  });

  it('reads a null review as under no review', () => {
    expect(narrowPlanInfo(planInfoWire({ review: null })).review).toBeNull();
  });

  it('reads a plan without a version key as version zero', () => {
    expect(narrowPlanInfo(planInfoWire()).version).toBe(0);
  });

  it('rejects a review outside the closed set rather than normalizing it', () => {
    expect(() => narrowPlanInfo(planInfoWire({ review: 'bogus' }))).toThrow(BoundaryError);
    expect(() => narrowPlanInfo(planInfoWire({ review: 1 }))).toThrow(BoundaryError);
  });

  it('rejects a version that is not a non-negative integer', () => {
    for (const version of [-1, 1.5, '3', null]) {
      expect(() => narrowPlanInfo(planInfoWire({ version }))).toThrow(BoundaryError);
    }
  });
});

describe('the review mark of a task', () => {
  function narrowedMark(reviewMark: unknown): ReviewMark | null | undefined {
    const info = narrowPlanInfo(
      planInfoWire({ tasks: [planTaskInfoWire({ review_mark: reviewMark })] })
    );
    return info.tasks[0]?.review_mark;
  }

  it('keeps each known mark', () => {
    const marks: ReviewMark[] = ['new', 'changed'];
    for (const mark of marks) {
      expect(narrowedMark(mark)).toBe(mark);
    }
  });

  it('reads a task without a review_mark key as unmarked', () => {
    expect(narrowPlanInfo(planInfoWire()).tasks[0]?.review_mark).toBeNull();
  });

  it('reads a null review_mark as unmarked', () => {
    expect(narrowedMark(null)).toBeNull();
  });

  it('rejects a mark outside the closed set rather than normalizing it', () => {
    for (const mark of ['bogus', 'New', 0]) {
      expect(() =>
        narrowPlanInfo(planInfoWire({ tasks: [planTaskInfoWire({ review_mark: mark })] }))
      ).toThrow(BoundaryError);
    }
  });
});

describe('the default assignment source', () => {
  it('accepts a task resolved from the default role', () => {
    const source: AssignmentSource = 'default';
    const info = narrowPlanInfo(
      planInfoWire({
        tasks: [
          planTaskInfoWire({
            role: 'docs',
            resolved: resolvedAssignmentWire({ source, assignment: taskAssignmentWire() }),
          }),
        ],
      })
    );
    expect(info.tasks[0]?.resolved.source).toBe('default');
  });
});

describe('the review state of a plan summary', () => {
  it('keeps the review and the unseen-changes flag the daemon reported', () => {
    const [pending, changed] = narrowPlanList({
      plans: [
        planSummaryWire({ review: 'pending', unseen_changes: false }),
        planSummaryWire({ plan_id: 'p-def456', review: 'approved', unseen_changes: true }),
      ],
    });
    expect(pending?.review).toBe('pending');
    expect(pending?.unseen_changes).toBe(false);
    expect(changed?.review).toBe('approved');
    expect(changed?.unseen_changes).toBe(true);
  });

  it('reads a row without the new keys as under no review and with nothing unseen', () => {
    const [row] = narrowPlanList({ plans: [planSummaryWire()] });
    expect(row?.review).toBeNull();
    expect(row?.unseen_changes).toBe(false);
  });

  it('rejects a review outside the closed set', () => {
    expect(() => narrowPlanList({ plans: [planSummaryWire({ review: 'bogus' })] })).toThrow(
      BoundaryError
    );
  });

  it('rejects an unseen_changes that is not a boolean', () => {
    for (const unseen of ['yes', 1, null]) {
      expect(() =>
        narrowPlanList({ plans: [planSummaryWire({ unseen_changes: unseen })] })
      ).toThrow(BoundaryError);
    }
  });
});

describe('the default role of a role table', () => {
  it('keeps the default role the daemon reported alongside the rows', () => {
    const table: RoleTable = narrowRoleList({
      roles: [roleEntryWire()],
      default_role: 'review',
    });
    expect(table.default_role).toBe('review');
    expect(table.roles.map((role) => role.name)).toEqual(['review']);
  });

  it('reads a table without a default_role key as having none', () => {
    expect(narrowRoleList({ roles: [roleEntryWire()] }).default_role).toBeNull();
  });

  it('reads a null default_role as having none', () => {
    expect(narrowRoleList({ roles: [], default_role: null }).default_role).toBeNull();
  });

  it('rejects a default_role that is not a string or null', () => {
    expect(() => narrowRoleList({ roles: [], default_role: 7 })).toThrow(BoundaryError);
  });
});

describe('the values a plan reply keeps', () => {
  it('keeps the roles warning, the task role, the user assignment and the resolved assignment', () => {
    const info = narrowPlanInfo(
      planInfoWire({
        roles_warning: 'roles.toml is missing',
        tasks: [
          planTaskInfoWire({
            role: 'review',
            user: taskAssignmentWire({ profile: 'codex-cli', model: 'm', effort: 'high' }),
            resolved: resolvedAssignmentWire({
              source: 'user',
              assignment: taskAssignmentWire({ profile: 'codex-cli', effort: 'high' }),
            }),
          }),
        ],
      })
    );
    expect(info.roles_warning).toBe('roles.toml is missing');
    const task = info.tasks[0];
    expect(task?.role).toBe('review');
    expect(task?.user).toEqual({ profile: 'codex-cli', model: 'm', effort: 'high' });
    expect(task?.resolved).toEqual({
      source: 'user',
      assignment: { profile: 'codex-cli', model: null, effort: 'high' },
    });
  });

  it('reads an omitted parent, user or resolved assignment as none, as the Rust Option decodes', () => {
    const task = planTaskInfoWire({ resolved: { source: 'unassigned' } });
    delete task['parent'];
    delete task['user'];
    const info = narrowPlanInfo(planInfoWire({ tasks: [task] }));
    expect(info.tasks[0]?.parent).toBeNull();
    expect(info.tasks[0]?.user).toBeNull();
    expect(info.tasks[0]?.resolved).toEqual({ source: 'unassigned', assignment: null });
  });

  it('keeps the owner window of a plan summary row', () => {
    const [row] = narrowPlanList({ plans: [planSummaryWire({ owner_window: 'window-b' })] });
    expect(row?.owner_window).toBe('window-b');
  });

  it('keeps the effort of a role row', () => {
    const table = narrowRoleList({ roles: [roleEntryWire({ model: 'm', effort: 'low' })] });
    expect(table.roles[0]).toEqual({
      name: 'review',
      profile: 'codex-cli',
      model: 'm',
      effort: 'low',
    });
  });
});

describe('a refusal says which part of the reply was wrong', () => {
  function planWith(overrides: Record<string, unknown>): unknown {
    return planInfoWire(overrides);
  }

  it('names the plan, its waves and its tasks', () => {
    expect(() => narrowPlanInfo(7)).toThrow(/^plan: expected an object/);
    expect(() => narrowPlanInfo(planWith({ waves: {} }))).toThrow(/^plan\.waves/);
    expect(() => narrowPlanInfo(planWith({ waves: [[1]] }))).toThrow(/^plan\.waves/);
    expect(() => narrowPlanInfo(planWith({ tasks: {} }))).toThrow(/^plan\.tasks/);
    expect(() => narrowPlanInfo(planWith({ tasks: [{}] }))).toThrow(/^plan\.tasks/);
  });

  it('names the task assignment that is wrong', () => {
    const bad = planTaskInfoWire({ user: { model: null } });
    expect(() => narrowPlanInfo(planWith({ tasks: [bad] }))).toThrow(/plan\.tasks\.user\.profile/);
    const badParent = planTaskInfoWire({ parent: 'x' });
    expect(() => narrowPlanInfo(planWith({ tasks: [badParent] }))).toThrow(
      /plan\.tasks\.parent: expected an object/
    );
  });

  it('names the resolved assignment and its unrecognized source', () => {
    const bad = planTaskInfoWire({ resolved: 'x' });
    expect(() => narrowPlanInfo(planWith({ tasks: [bad] }))).toThrow(/plan\.tasks\.resolved/);
    const source = planTaskInfoWire({ resolved: resolvedAssignmentWire({ source: 'bogus' }) });
    expect(() => narrowPlanInfo(planWith({ tasks: [source] }))).toThrow(
      /resolved\.source: unrecognized source "bogus"/
    );
  });

  it('names the report and its unrecognized outcome', () => {
    const notObject = planTaskInfoWire({ report: 'x' });
    expect(() => narrowPlanInfo(planWith({ tasks: [notObject] }))).toThrow(/plan\.tasks\.report/);
    const outcome = planTaskInfoWire({ report: taskReportWire({ outcome: 'bogus' }) });
    expect(() => narrowPlanInfo(planWith({ tasks: [outcome] }))).toThrow(
      /report\.outcome: unrecognized outcome "bogus"/
    );
  });

  it('names the review value that is not in the closed set', () => {
    expect(() => narrowPlanInfo(planWith({ review: 'bogus' }))).toThrow(
      /plan\.review: unrecognized value "bogus"/
    );
  });

  it('names the plan list, its rows and the flag that is not a boolean', () => {
    expect(() => narrowPlanList(1)).toThrow(/^list_plans/);
    expect(() => narrowPlanList({ plans: 1 })).toThrow(/^list_plans\.plans/);
    expect(() => narrowPlanList({ plans: [1] })).toThrow(/^list_plans\.plans/);
    expect(() => narrowPlanList({ plans: [planSummaryWire({ unseen_changes: null })] })).toThrow(
      /list_plans\.plans\.unseen_changes: expected a boolean/
    );
  });

  it('names the role list and its rows', () => {
    expect(() => narrowRoleList(1)).toThrow(/^list_roles/);
    expect(() => narrowRoleList({ roles: 1 })).toThrow(/^list_roles\.roles/);
    expect(() => narrowRoleList({ roles: [1] })).toThrow(/^list_roles\.roles/);
  });
});
