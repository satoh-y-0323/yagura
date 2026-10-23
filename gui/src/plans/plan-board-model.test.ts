/**
 * The plans board's display logic, measured as pure functions: the wave
 * columns laid out from the daemon's own grouping, the assignment source and
 * the report outcome shown on a card, which windows are alive and what they
 * are called (read from the GUI's window list rather than from the plan),
 * when to read the plan again, what the assignment editor and the owner
 * picker offer, and the review gate: which banner a plan shows, which edits
 * are still unapplied, when the rail flags the plans, and when the board
 * tells the daemon a plan was seen.
 */
import { describe, expect, it } from 'vitest';
import { get } from 'svelte/store';

import type {
  AssignmentSource,
  PlanInfo,
  PlanSummary,
  PlanTaskInfo,
  TaskAssignment,
} from '../ipc/plan-boundary';
import type { ProfileEntry, ProfileLimit, WindowInfo } from '../ipc/session-client';
import { plans } from '../ui-messages';
import { createWindowRegistry, type WindowEntry } from '../stores/window-registry';

import {
  assignmentFromEditor,
  assignmentSourceKey,
  buildWaveColumns,
  isUnassigned,
  isWindowLive,
  ownerCandidates,
  planOwner,
  planReloadFor,
  plansAttention,
  reportOutcomeKey,
  resolveBoardWindow,
  reviewBannerKind,
  reviewMarksOf,
  selectionAfterPlanReadFailure,
  shouldMarkPlanSeen,
  taskOpenRefusal,
  unappliedTaskIds,
  withRetainedMarks,
  type BoardWindow,
  type BoardWindowState,
  type EditorFields,
  type WaveColumn,
} from './plan-board-model';

function listed(
  windowId: string,
  name: string,
  status: 'running' | 'idle' | 'exited'
): BoardWindow {
  return { windowId, name, detected: { status } };
}

function unknownWindow(windowId: string, name: string): BoardWindow {
  return { windowId, name, detected: { status: 'unknown', reason: null } };
}

const CLAUDE: TaskAssignment = { profile: 'claude', model: 'opus', effort: 'high' };

function task(taskId: string, wave: number, overrides: Partial<PlanTaskInfo> = {}): PlanTaskInfo {
  return {
    task_id: taskId,
    title: `Task ${taskId}`,
    role: null,
    depends_on: [],
    writes: [],
    wave,
    parent: null,
    user: null,
    resolved: { source: 'parent', assignment: CLAUDE },
    window_id: null,
    report: null,
    review_mark: null,
    has_instruction: false,
    ...overrides,
  };
}

function plan(overrides: Partial<PlanInfo> = {}): PlanInfo {
  return {
    plan_id: 'plan-a',
    title: 'Ship the ledger',
    cwd: 'C:\\work\\repo',
    owner_window: 'w-owner',
    waves: [['alpha'], ['beta']],
    tasks: [task('alpha', 0), task('beta', 1, { depends_on: ['alpha'] })],
    roles_warning: null,
    review: null,
    version: 0,
    ...overrides,
  };
}

function cardIds(columns: WaveColumn[]): string[][] {
  return columns.map((column) => column.cards.map((card) => card.task.task_id));
}

describe('wave columns follow the daemon grouping', () => {
  it('lays out columns in the order of waves and cards in the order each wave lists them', () => {
    const detail = plan({
      owner_window: null,
      waves: [['gamma', 'alpha'], ['delta'], ['beta', 'epsilon']],
      tasks: [
        task('alpha', 0),
        task('beta', 2),
        task('gamma', 0),
        task('delta', 1),
        task('epsilon', 2),
      ],
    });

    const columns = buildWaveColumns(detail, []);

    expect(cardIds(columns)).toEqual([['gamma', 'alpha'], ['delta'], ['beta', 'epsilon']]);
    expect(columns.map((column) => column.wave)).toEqual([0, 1, 2]);
  });

  it('carries each task as the daemon reported it', () => {
    const beta = task('beta', 1, {
      role: 'reviewer',
      writes: ['src/lib.rs'],
      depends_on: ['alpha'],
      user: { profile: 'codex', model: null, effort: 'medium' },
      resolved: { source: 'user', assignment: { profile: 'codex', model: null, effort: 'medium' } },
    });
    const detail = plan({ tasks: [task('alpha', 0), beta] });

    const columns = buildWaveColumns(detail, []);

    expect(columns[1]?.cards[0]?.task).toEqual(beta);
  });

  it('yields no columns for a plan without waves', () => {
    expect(buildWaveColumns(plan({ waves: [], tasks: [] }), [])).toEqual([]);
  });
});

describe('assignment source display', () => {
  const sources: AssignmentSource[] = ['user', 'parent', 'role', 'unassigned'];

  it('gives each source one non-empty key of its own', () => {
    const keys = sources.map((source) => assignmentSourceKey(source));

    for (const key of keys) {
      expect(typeof key).toBe('string');
      expect(key.length).toBeGreaterThan(0);
    }
    expect(new Set(keys).size).toBe(sources.length);
  });

  it('tells an unassigned task from an assigned one', () => {
    expect(isUnassigned({ source: 'unassigned', assignment: null })).toBe(true);
    for (const source of ['user', 'parent', 'role'] as const) {
      expect(isUnassigned({ source, assignment: CLAUDE })).toBe(false);
    }
  });
});

describe('report outcome display', () => {
  it('labels a succeeded report with the reportSucceeded plans message', () => {
    expect(reportOutcomeKey('succeeded')).toBe('reportSucceeded');
  });

  it('labels a failed report with the reportFailed plans message', () => {
    expect(reportOutcomeKey('failed')).toBe('reportFailed');
  });

  it('names only keys the plans messages define', () => {
    const plansKeys = Object.keys(plans);
    for (const outcome of ['succeeded', 'failed'] as const) {
      expect(plansKeys).toContain(reportOutcomeKey(outcome));
    }
  });
});

describe('window liveness is read from the window list', () => {
  it('counts a listed window that has not exited as live', () => {
    const windows = [listed('w-first', 'one', 'running'), unknownWindow('w-second', 'two')];

    expect(isWindowLive(windows, 'w-first')).toBe(true);
    expect(isWindowLive(windows, 'w-second')).toBe(true);
  });

  it('counts a missing id, an unlisted window and an exited one as not live', () => {
    const windows = [listed('w-first', 'one', 'exited')];

    expect(isWindowLive(windows, null)).toBe(false);
    expect(isWindowLive(windows, 'w-ghost')).toBe(false);
    expect(isWindowLive(windows, 'w-first')).toBe(false);
  });
});

describe('the plan owner', () => {
  it('shows a live owner with the name the window list gives it', () => {
    const windows = [listed('w-other', 'other', 'idle'), listed('w-owner', 'planner', 'running')];

    expect(planOwner(plan(), windows)).toEqual({
      kind: 'live',
      windowId: 'w-owner',
      name: 'planner',
      detected: { status: 'running' },
    });
  });

  it('shows no owner when the plan has none', () => {
    const windows = [listed('w-owner', 'planner', 'running')];

    expect(planOwner(plan({ owner_window: null }), windows)).toEqual({ kind: 'none' });
  });

  it('shows no owner when the owner is not in the window list', () => {
    expect(planOwner(plan(), [listed('w-other', 'other', 'running')])).toEqual({
      kind: 'none',
    });
  });

  it('shows no owner when the owner has exited', () => {
    expect(planOwner(plan(), [listed('w-owner', 'planner', 'exited')])).toEqual({
      kind: 'none',
    });
  });

  it('follows a change of the window list alone, without the plan being read again', () => {
    const detail = plan();
    const before = structuredClone(detail);

    const live = planOwner(detail, [listed('w-owner', 'planner', 'running')]);
    const exited = planOwner(detail, [listed('w-owner', 'planner', 'exited')]);
    const gone = planOwner(detail, []);
    const back = planOwner(detail, [listed('w-owner', 'renamed', 'idle')]);

    expect(live.kind).toBe('live');
    expect(exited).toEqual({ kind: 'none' });
    expect(gone).toEqual({ kind: 'none' });
    expect(back).toEqual({
      kind: 'live',
      windowId: 'w-owner',
      name: 'renamed',
      detected: { status: 'idle' },
    });
    expect(detail).toEqual(before);
  });

  it('agrees with the liveness rule for every window list', () => {
    const lists: BoardWindow[][] = [
      [],
      [listed('w-owner', 'planner', 'running')],
      [listed('w-owner', 'planner', 'exited')],
      [unknownWindow('w-owner', 'planner')],
      [listed('w-other', 'other', 'running')],
    ];
    for (const windows of lists) {
      const owner = planOwner(plan(), windows);
      expect(owner.kind === 'live').toBe(isWindowLive(windows, 'w-owner'));
    }
  });
});

describe('the window bound to a task card', () => {
  function cardWindow(windowId: string | null, windows: readonly BoardWindow[]): BoardWindowState {
    const detail = plan({ waves: [['alpha']], tasks: [task('alpha', 0, { window_id: windowId })] });
    const card = buildWaveColumns(detail, windows)[0]?.cards[0];
    if (card === undefined) {
      throw new Error('the plan should yield one card');
    }
    return card.window;
  }

  it('names a live bound window from the window list', () => {
    const windows = [listed('w-task', 'implementer', 'running')];

    expect(cardWindow('w-task', windows)).toEqual({
      kind: 'live',
      windowId: 'w-task',
      name: 'implementer',
      detected: { status: 'running' },
    });
  });

  it('shows no window for an unlisted id, an exited window, or a task never opened', () => {
    expect(cardWindow('w-task', [listed('w-other', 'other', 'running')])).toEqual({ kind: 'none' });
    expect(cardWindow('w-task', [listed('w-task', 'implementer', 'exited')])).toEqual({
      kind: 'none',
    });
    expect(cardWindow(null, [listed('w-task', 'implementer', 'running')])).toEqual({
      kind: 'none',
    });
  });

  it('resolves each card against the window list on its own', () => {
    const detail = plan({
      owner_window: null,
      waves: [['alpha', 'beta', 'gamma']],
      tasks: [
        task('alpha', 0, { window_id: 'w-a' }),
        task('beta', 0, { window_id: 'w-b' }),
        task('gamma', 0, { window_id: 'w-c' }),
      ],
    });
    const windows = [listed('w-c', 'third', 'idle'), listed('w-b', 'second', 'exited')];

    const cards = buildWaveColumns(detail, windows)[0]?.cards ?? [];

    expect(cards.map((card) => card.window)).toEqual([
      { kind: 'none' },
      { kind: 'none' },
      { kind: 'live', windowId: 'w-c', name: 'third', detected: { status: 'idle' } },
    ]);
  });

  it('uses the same rule as the owner', () => {
    const lists: BoardWindow[][] = [
      [],
      [listed('w-x', 'x', 'running')],
      [listed('w-x', 'x', 'exited')],
      [unknownWindow('w-x', 'x')],
    ];
    for (const windows of lists) {
      const card = cardWindow('w-x', windows);
      expect(card).toEqual(resolveBoardWindow(windows, 'w-x'));
      expect(card).toEqual(planOwner(plan({ owner_window: 'w-x' }), windows));
      expect(card.kind === 'live').toBe(isWindowLive(windows, 'w-x'));
    }
  });
});

describe('when the board reads again', () => {
  it('reads the plan list too, so the selector reflects a title change without reopening the panel', () => {
    const reload = planReloadFor({ kind: 'plan-changed', planId: 'plan-a' }, 'plan-a');

    expect(reload).toEqual({ list: true, roles: false, planId: 'plan-a' });
  });

  it('reads only the list when the push names another plan', () => {
    expect(planReloadFor({ kind: 'plan-changed', planId: 'plan-b' }, 'plan-a')).toEqual({
      list: true,
      roles: false,
      planId: null,
    });
  });

  it('reads only the list when a push arrives while no plan is shown', () => {
    expect(planReloadFor({ kind: 'plan-changed', planId: 'plan-b' }, null)).toEqual({
      list: true,
      roles: false,
      planId: null,
    });
  });

  it('reads both the role table and the shown plan after the table is saved', () => {
    const reload = planReloadFor({ kind: 'roles-saved' }, 'plan-a');

    expect(reload.roles).toBe(true);
    expect(reload.planId).toBe('plan-a');
  });

  it('reads the role table alone after a save while no plan is shown', () => {
    const reload = planReloadFor({ kind: 'roles-saved' }, null);

    expect(reload.roles).toBe(true);
    expect(reload.planId).toBeNull();
  });

  it('reads nothing when only the window list changes', () => {
    expect(planReloadFor({ kind: 'window-list-changed' }, 'plan-a')).toEqual({
      list: false,
      roles: false,
      planId: null,
    });
  });

  it('drops the selection when the shown plan no longer exists', () => {
    const failure = selectionAfterPlanReadFailure;

    expect(
      failure('plan-a', 'plan-a', 'the daemon rejected the request: plan_not_found: no such plan')
    ).toBeNull();
    expect(
      failure('plan-a', 'plan-a', 'the daemon rejected the request: plan_not_found')
    ).toBeNull();
  });

  it('keeps the selection when the read failed for another reason', () => {
    const failure = selectionAfterPlanReadFailure;

    expect(failure('plan-a', 'plan-a', 'not connected to the yagura daemon')).toBe('plan-a');
    expect(
      failure('plan-a', 'plan-a', 'the daemon rejected the request: invalid_state: plan_not_found')
    ).toBe('plan-a');
    expect(failure('plan-a', 'plan-a', new Error('plan: expected a record'))).toBe('plan-a');
  });

  it('keeps a selection made after the failed read was sent', () => {
    expect(
      selectionAfterPlanReadFailure(
        'plan-b',
        'plan-a',
        'the daemon rejected the request: plan_not_found'
      )
    ).toBe('plan-b');
  });
});

describe('the assignment editor', () => {
  it('keeps text that is not one of the aliases as entered', () => {
    expect(
      assignmentFromEditor({ profile: 'codex', model: 'my-own-model', effort: 'xhigh' })
    ).toEqual({ profile: 'codex', model: 'my-own-model', effort: 'xhigh' });
    expect(assignmentFromEditor({ profile: 'claude', model: 'opus', effort: '' })).toEqual({
      profile: 'claude',
      model: 'opus',
      effort: null,
    });
  });
});

describe('owner candidates come from the window list', () => {
  // The plan names only windows that are gone, so a candidate list built from
  // the plan could not match the one built from the window list.
  const detail = plan({
    owner_window: 'w-gone',
    waves: [['alpha']],
    tasks: [task('alpha', 0, { window_id: 'w-closed' })],
  });

  it('lists the live windows in window-list order, with their names', () => {
    const windows = [
      listed('w-second', 'second', 'idle'),
      listed('w-closed', 'closed', 'exited'),
      unknownWindow('w-first', 'first'),
      listed('w-third', 'third', 'running'),
    ];

    expect(ownerCandidates(detail, windows)).toEqual([
      { windowId: 'w-second', name: 'second' },
      { windowId: 'w-first', name: 'first' },
      { windowId: 'w-third', name: 'third' },
    ]);
  });

  it('offers a window that joins the list after the plan was read', () => {
    const before = [listed('w-first', 'first', 'running')];
    const after = [...before, listed('w-new', 'newcomer', 'running')];

    expect(ownerCandidates(detail, before)).toEqual([{ windowId: 'w-first', name: 'first' }]);
    expect(ownerCandidates(detail, after)).toEqual([
      { windowId: 'w-first', name: 'first' },
      { windowId: 'w-new', name: 'newcomer' },
    ]);
  });

  it('drops a window that exits or leaves the list', () => {
    const before = [listed('w-first', 'first', 'running'), listed('w-second', 'second', 'running')];

    expect(
      ownerCandidates(detail, [
        listed('w-first', 'first', 'exited'),
        listed('w-second', 'second', 'running'),
      ])
    ).toEqual([{ windowId: 'w-second', name: 'second' }]);
    expect(ownerCandidates(detail, before.slice(0, 1))).toEqual([
      { windowId: 'w-first', name: 'first' },
    ]);
  });

  it('offers nothing when no window is live', () => {
    expect(ownerCandidates(detail, [])).toEqual([]);
  });
});

describe('the window registry feeds the board directly', () => {
  function info(windowId: string, name: string): WindowInfo {
    return {
      window_id: windowId,
      name,
      vendor: null,
      cwd: 'C:\\work\\repo',
      detected: { status: 'running' },
      exit_code: null,
    };
  }

  it('drops an owner and a candidate once the registry records the exit', () => {
    const registry = createWindowRegistry();
    registry.openWindow(info('w-owner', 'planner'));
    registry.openWindow(info('w-helper', 'helper'));
    const detail = plan();

    const openList: WindowEntry[] = get(registry).windows;
    expect(planOwner(detail, openList).kind).toBe('live');
    expect(ownerCandidates(detail, openList)).toEqual([
      { windowId: 'w-owner', name: 'planner' },
      { windowId: 'w-helper', name: 'helper' },
    ]);

    registry.setStatus('w-owner', { status: 'exited' }, null);
    const afterExit: WindowEntry[] = get(registry).windows;
    expect(planOwner(detail, afterExit)).toEqual({ kind: 'none' });
    expect(ownerCandidates(detail, afterExit)).toEqual([{ windowId: 'w-helper', name: 'helper' }]);
  });
});

describe('the review banner', () => {
  it('asks for approval while the plan is pending', () => {
    expect(reviewBannerKind(plan({ review: 'pending' }))).toBe('pending');
  });

  it('shows the plan as approved when no task carries a mark', () => {
    expect(reviewBannerKind(plan({ review: 'approved' }))).toBe('approved');
  });

  it('shows the plan as changed once any task carries a mark after approval', () => {
    for (const mark of ['new', 'changed'] as const) {
      const detail = plan({
        review: 'approved',
        tasks: [task('alpha', 0), task('beta', 1, { review_mark: mark })],
      });
      expect(reviewBannerKind(detail)).toBe('changed');
    }
  });

  it('shows no banner for a plan under no review', () => {
    expect(reviewBannerKind(plan({ review: null }))).toBeNull();
  });
});

describe('unapplied assignment edits', () => {
  const OTHER: EditorFields = { profile: 'codex', model: '', effort: 'medium' };
  const SAME_AS_RESOLVED: EditorFields = { profile: 'claude', model: 'opus', effort: 'high' };

  function discard(
    drafts: Record<string, EditorFields>,
    taskId: string
  ): Record<string, EditorFields> {
    return Object.fromEntries(Object.entries(drafts).filter(([key]) => key !== taskId));
  }

  it('finds nothing when no task is being edited', () => {
    expect(unappliedTaskIds(plan(), {})).toEqual([]);
  });

  it('finds a task whose edit differs from the assignment it shows', () => {
    expect(unappliedTaskIds(plan(), { beta: OTHER })).toEqual(['beta']);
  });

  it('finds an edit that differs in one field only', () => {
    const drafts = { alpha: { ...SAME_AS_RESOLVED, effort: 'low' } };
    expect(unappliedTaskIds(plan(), drafts)).toEqual(['alpha']);
  });

  it('compares by value, so an edit changed back to the shown assignment is not unapplied', () => {
    expect(unappliedTaskIds(plan(), { alpha: { ...SAME_AS_RESOLVED } })).toEqual([]);
  });

  it('stops counting an edit once it is discarded', () => {
    const drafts: Record<string, EditorFields> = { alpha: OTHER, beta: OTHER };
    expect([...unappliedTaskIds(plan(), drafts)].sort()).toEqual(['alpha', 'beta']);

    expect(unappliedTaskIds(plan(), discard(drafts, 'alpha'))).toEqual(['beta']);
  });

  it('reads blank fields as the absence of an assignment on an unassigned task', () => {
    const detail = plan({
      tasks: [
        task('alpha', 0, { resolved: { source: 'unassigned', assignment: null } }),
        task('beta', 1),
      ],
    });
    const blank: EditorFields = { profile: '', model: '', effort: '' };

    expect(unappliedTaskIds(detail, { alpha: blank })).toEqual([]);
    expect(unappliedTaskIds(detail, { alpha: { ...blank, profile: 'claude' } })).toEqual(['alpha']);
  });

  it('reads an unset launch option as blank text', () => {
    const detail = plan({
      waves: [['alpha']],
      tasks: [
        task('alpha', 0, {
          resolved: { source: 'role', assignment: { profile: 'codex', model: null, effort: null } },
        }),
      ],
    });

    expect(
      unappliedTaskIds(detail, { alpha: { profile: 'codex', model: '', effort: '' } })
    ).toEqual([]);
  });

  it('ignores an edit left for a task the plan no longer has', () => {
    expect(unappliedTaskIds(plan(), { gone: OTHER })).toEqual([]);
  });
});

describe('the plans flag on the rail', () => {
  function summary(planId: string, overrides: Partial<PlanSummary> = {}): PlanSummary {
    return {
      plan_id: planId,
      title: `Plan ${planId}`,
      owner_window: null,
      task_count: 1,
      review: null,
      unseen_changes: false,
      ...overrides,
    };
  }

  it('flags nothing when there are no plans', () => {
    expect(plansAttention([])).toBeNull();
  });

  it('flags nothing when every plan is settled and seen', () => {
    const summaries = [summary('a'), summary('b', { review: 'approved' })];
    expect(plansAttention(summaries)).toBeNull();
  });

  it('counts the plans waiting for approval', () => {
    const summaries = [
      summary('a', { review: 'pending' }),
      summary('b', { review: 'approved' }),
      summary('c', { review: 'pending' }),
    ];
    expect(plansAttention(summaries)).toEqual({ kind: 'pending', count: 2 });
  });

  it('counts the plans with unseen changes when none waits for approval', () => {
    const summaries = [
      summary('a', { review: 'approved', unseen_changes: true }),
      summary('b', { review: 'approved' }),
      summary('c', { review: 'approved', unseen_changes: true }),
      summary('d', { review: 'approved', unseen_changes: true }),
    ];
    expect(plansAttention(summaries)).toEqual({ kind: 'changed', count: 3 });
  });

  it('puts plans waiting for approval first when both kinds are present', () => {
    const summaries = [
      summary('a', { review: 'approved', unseen_changes: true }),
      summary('b', { review: 'pending' }),
      summary('c', { review: 'approved', unseen_changes: true }),
    ];
    expect(plansAttention(summaries)).toEqual({ kind: 'pending', count: 1 });
  });
});

describe('telling the daemon a plan was seen', () => {
  const marked = plan({
    review: 'approved',
    tasks: [task('alpha', 0, { review_mark: 'changed' }), task('beta', 1)],
  });
  const unmarked = plan({ review: 'approved' });

  it('sends it when the user opens a plan that carries marks', () => {
    expect(shouldMarkPlanSeen('open', marked)).toBe(true);
  });

  it('sends it for a new task as well as a changed one', () => {
    const added = plan({
      review: 'approved',
      tasks: [task('alpha', 0), task('beta', 1, { review_mark: 'new' })],
    });
    expect(shouldMarkPlanSeen('open', added)).toBe(true);
  });

  it('does not send it when the opened plan carries no mark', () => {
    expect(shouldMarkPlanSeen('open', unmarked)).toBe(false);
  });

  it('does not send it when a push made the board read the plan again', () => {
    expect(shouldMarkPlanSeen('push', marked)).toBe(false);
    expect(shouldMarkPlanSeen('push', unmarked)).toBe(false);
  });
});

describe('the marks kept on screen after the user has seen them', () => {
  const marked = plan({
    review: 'approved',
    tasks: [task('alpha', 0, { review_mark: 'changed' }), task('beta', 1, { review_mark: 'new' })],
  });
  const cleared = plan({ review: 'approved' });

  it('collects the mark of every marked task', () => {
    expect(reviewMarksOf(marked)).toEqual({ alpha: 'changed', beta: 'new' });
    expect(reviewMarksOf(cleared)).toEqual({});
  });

  it('puts a retained mark back on a task the daemon has since cleared', () => {
    const shown = withRetainedMarks(cleared, reviewMarksOf(marked));
    expect(shown.tasks.map((t) => t.review_mark)).toEqual(['changed', 'new']);
    expect(reviewBannerKind(shown)).toBe('changed');
  });

  it("keeps the daemon's own mark over a retained one, and ignores a retained task no longer in the plan", () => {
    const current = plan({
      review: 'approved',
      tasks: [task('alpha', 0, { review_mark: 'new' }), task('beta', 1)],
    });
    const shown = withRetainedMarks(current, { alpha: 'changed', gone: 'new' });
    expect(shown.tasks.map((t) => t.review_mark)).toEqual(['new', null]);
  });

  it('leaves the plan untouched when nothing is retained', () => {
    expect(withRetainedMarks(cleared, {})).toEqual(cleared);
  });
});

describe('the default assignment source', () => {
  it('has a plans message key of its own, apart from every other source', () => {
    const others: AssignmentSource[] = ['user', 'parent', 'role', 'unassigned'];
    const key = assignmentSourceKey('default');

    expect(Object.keys(plans)).toContain(key);
    expect(others.map((source) => assignmentSourceKey(source))).not.toContain(key);
  });
});

describe('the board against input the daemon should not send', () => {
  it('leaves out a task a wave names but the plan does not hold', () => {
    const columns = buildWaveColumns(plan({ waves: [['alpha', 'ghost'], ['beta']] }), []);

    expect(cardIds(columns)).toEqual([['alpha'], ['beta']]);
  });

  it('finds no live window for a null id even when the list holds a window with an odd id', () => {
    expect(resolveBoardWindow([listed('null', 'odd', 'running')], null)).toEqual({ kind: 'none' });
  });
});

describe('reading again after a push', () => {
  it('reads the list and the shown plan for a push that names the shown plan', () => {
    expect(planReloadFor({ kind: 'plan-changed', planId: 'plan-a' }, 'plan-a')).toEqual({
      list: true,
      roles: false,
      planId: 'plan-a',
    });
  });

  it('reads the list alone for a push that names another plan', () => {
    expect(planReloadFor({ kind: 'plan-changed', planId: 'plan-b' }, 'plan-a')).toEqual({
      list: true,
      roles: false,
      planId: null,
    });
  });

  it('reads the table and the shown plan after the table was saved', () => {
    expect(planReloadFor({ kind: 'roles-saved' }, 'plan-a')).toEqual({
      list: false,
      roles: true,
      planId: 'plan-a',
    });
  });
});

describe('dropping a selection after a failed read', () => {
  const rejected = (code: string, rest = ''): string =>
    `the daemon rejected the request: ${code}${rest}`;

  it('drops it when the daemon refused the plan as not found, whether the error is text or an Error', () => {
    expect(selectionAfterPlanReadFailure('p', 'p', rejected('plan_not_found'))).toBeNull();
    expect(
      selectionAfterPlanReadFailure('p', 'p', new Error(rejected('plan_not_found')))
    ).toBeNull();
    expect(
      selectionAfterPlanReadFailure('p', 'p', rejected('plan_not_found', ': gone'))
    ).toBeNull();
  });

  it.each<[string, unknown]>([
    ['another code', rejected('invalid_state', ': plan_not_found')],
    ['text that is not a rejection', 'plan_not_found'],
    ['a number', 42],
    ['null', null],
    ['an object', { code: 'plan_not_found' }],
  ])('keeps it for %s', (_label, error) => {
    expect(selectionAfterPlanReadFailure('p', 'p', error)).toBe('p');
  });

  it('keeps no selection when there was none', () => {
    expect(selectionAfterPlanReadFailure(null, 'p', rejected('plan_not_found'))).toBeNull();
  });
});

describe('the assignment editor fields', () => {
  it('reads blank model and effort as not set, and keeps the profile as typed', () => {
    expect(assignmentFromEditor({ profile: 'claude', model: '', effort: '' })).toEqual({
      profile: 'claude',
      model: null,
      effort: null,
    });
    expect(assignmentFromEditor({ profile: 'claude', model: 'm', effort: '' })).toEqual({
      profile: 'claude',
      model: 'm',
      effort: null,
    });
    expect(assignmentFromEditor({ profile: 'claude', model: '', effort: 'e' })).toEqual({
      profile: 'claude',
      model: null,
      effort: 'e',
    });
  });
});

describe('unapplied edits: one field at a time', () => {
  const shown = { profile: 'claude', model: 'opus', effort: 'high' };

  it.each<[string, Partial<EditorFields>]>([
    ['profile', { profile: 'codex' }],
    ['model', { model: 'sonnet' }],
    ['effort', { effort: 'low' }],
  ])('finds a task whose %s alone was edited', (_field, change) => {
    expect(unappliedTaskIds(plan(), { alpha: { ...shown, ...change } })).toEqual(['alpha']);
  });

  it('finds nothing for an edit equal to what the task shows', () => {
    expect(unappliedTaskIds(plan(), { alpha: shown })).toEqual([]);
  });
});

describe('retained marks', () => {
  it('gives back the very plan when there are no marks to retain', () => {
    const current = plan();

    expect(withRetainedMarks(current, {})).toBe(current);
  });
});

function launchProfile(id: string, limits: ProfileLimit['capability'][]): ProfileEntry {
  return {
    id,
    displayName: `${id} name`,
    detection: null,
    detectionWarning: null,
    cwd: null,
    command: [id],
    available: true,
    reason: null,
    unavailableKind: null,
    guidance: null,
    model: null,
    effort: null,
    enabled: true,
    limits: limits.map((capability) => ({
      capability,
      reason: `the ${capability} cannot pass a .cmd file unchanged`,
      guidance: { text: 'Use the WSL profile.', command: null, url: null },
    })),
  };
}

describe('taskOpenRefusal', () => {
  const profiles = [
    launchProfile('muse-code', ['effort', 'launch_prompt']),
    launchProfile('effort-only', ['effort']),
    launchProfile('claude', []),
  ];

  function assignedTo(profile: string, hasInstruction = true): PlanTaskInfo {
    return task('alpha', 0, {
      resolved: { source: 'role', assignment: { profile, model: null, effort: null } },
      has_instruction: hasInstruction,
    });
  }

  it('refuses to open a task whose profile cannot take its instruction at launch, in one line', () => {
    const refusal = taskOpenRefusal(assignedTo('muse-code'), profiles);

    expect(refusal).not.toBeNull();
    expect(refusal?.length).toBeGreaterThan(0);
    expect(refusal).not.toContain('\n');
  });

  it('lets a task with no instruction open whatever its profile takes at launch', () => {
    expect(taskOpenRefusal(assignedTo('muse-code', false), profiles)).toBeNull();
  });

  it('lets a task open when its profile carries the instruction at launch', () => {
    expect(taskOpenRefusal(assignedTo('effort-only'), profiles)).toBeNull();
    expect(taskOpenRefusal(assignedTo('claude'), profiles)).toBeNull();
  });

  it('leaves an unassigned task, or one whose profile the list does not hold, to the daemon', () => {
    expect(
      taskOpenRefusal(
        task('alpha', 0, { resolved: { source: 'unassigned', assignment: null } }),
        profiles
      )
    ).toBeNull();
    expect(taskOpenRefusal(assignedTo('gone'), profiles)).toBeNull();
  });
});
