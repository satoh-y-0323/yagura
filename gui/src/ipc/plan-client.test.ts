/**
 * TS-boundary contract for the plan-ledger bridge: the ten Tauri commands
 * the WebView calls (`list_plans`, `get_plan`, `delete_plan`,
 * `set_task_assignment`, `clear_task_assignment`, `set_plan_owner`,
 * `approve_plan`, `mark_plan_seen`, `list_roles`, `save_roles`) and the subscription to `session://plan-changed`
 * (`SESSION_EVENT.planChanged`).
 *
 * `@tauri-apps/api/core` and `@tauri-apps/api/event` are mocked, so every
 * assertion is about the calls this module makes and the shape it produces,
 * not about a real Tauri runtime. Lives in its own file next to
 * `plan-boundary.test.ts`, the way `session-client.chat.test.ts` sits next
 * to `session-boundary.chat-matrix.test.ts`.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const { invokeMock, listenMock, unlistenMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listenMock: vi.fn(),
  unlistenMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: listenMock,
}));

import { BoundaryError, SESSION_EVENT } from './session-boundary';
import type { PlanInfo, RoleEntry, RoleTable, TaskAssignment } from './plan-boundary';
import {
  invokeApprovePlan,
  invokeClearTaskAssignment,
  invokeDeletePlan,
  invokeGetPlan,
  invokeListPlans,
  invokeListRoles,
  invokeMarkPlanSeen,
  invokeSaveRoles,
  invokeSetPlanOwner,
  invokeSetTaskAssignment,
  subscribePlanChanged,
} from './plan-client';

/** A Tauri event callback, as registered through the mocked `listen`. */
type ListenCallback = (event: { payload: unknown }) => void;

/** A well-formed `TaskAssignment` wire value. */
function taskAssignmentWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    profile: 'claude-code',
    model: null,
    effort: null,
    ...overrides,
  };
}

/** A well-formed `PlanInfo` wire value, as `get_plan` (and friends) reply with it. */
function planInfoWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    plan_id: 'p-abc123',
    title: 'Plan window launcher',
    cwd: 'C:\\repo',
    owner_window: 'window-a',
    waves: [['investigate']],
    tasks: [
      {
        task_id: 'investigate',
        title: 'Investigate',
        role: 'review',
        depends_on: [],
        writes: ['gui/src/App.svelte'],
        wave: 0,
        parent: taskAssignmentWire(),
        user: null,
        resolved: { source: 'role', assignment: taskAssignmentWire() },
        window_id: 'window-a',
      },
    ],
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

beforeEach(() => {
  invokeMock.mockReset();
  listenMock.mockReset();
  unlistenMock.mockReset();
});

describe('invokeListPlans', () => {
  it('calls list_plans with no arguments', async () => {
    invokeMock.mockResolvedValueOnce({ plans: [planSummaryWire()] });

    await invokeListPlans();

    expect(invokeMock).toHaveBeenCalledWith('list_plans');
  });

  it('narrows the reply into the plan rows', async () => {
    invokeMock.mockResolvedValueOnce({ plans: [planSummaryWire({ plan_id: 'p-2b7c1d' })] });

    const plans = await invokeListPlans();

    expect(plans).toHaveLength(1);
    expect(plans[0]?.plan_id).toBe('p-2b7c1d');
  });

  it('rejects with BoundaryError when the reply fails validation', async () => {
    invokeMock.mockResolvedValueOnce({ plans: [planSummaryWire({ task_count: -1 })] });

    await expect(invokeListPlans()).rejects.toThrow(BoundaryError);
  });
});

describe('invokeGetPlan', () => {
  it('calls get_plan with the plan id', async () => {
    invokeMock.mockResolvedValueOnce(planInfoWire());

    await invokeGetPlan('p-abc123');

    expect(invokeMock).toHaveBeenCalledWith('get_plan', { plan_id: 'p-abc123' });
  });

  it('narrows the reply into a PlanInfo', async () => {
    invokeMock.mockResolvedValueOnce(planInfoWire({ title: 'Renamed' }));

    const plan: PlanInfo = await invokeGetPlan('p-abc123');

    expect(plan.title).toBe('Renamed');
  });

  it('rejects with BoundaryError when the reply fails validation', async () => {
    const wire = planInfoWire();
    delete wire['plan_id'];
    invokeMock.mockResolvedValueOnce(wire);

    await expect(invokeGetPlan('p-abc123')).rejects.toThrow(BoundaryError);
  });
});

describe('invokeDeletePlan', () => {
  it('calls delete_plan with the plan id', async () => {
    invokeMock.mockResolvedValueOnce('p-abc123');

    await invokeDeletePlan('p-abc123');

    expect(invokeMock).toHaveBeenCalledWith('delete_plan', { plan_id: 'p-abc123' });
  });

  it('resolves the deleted plan id', async () => {
    invokeMock.mockResolvedValueOnce('p-abc123');

    const deleted = await invokeDeletePlan('p-abc123');

    expect(deleted).toBe('p-abc123');
  });

  it('rejects with BoundaryError when the reply is not a string', async () => {
    invokeMock.mockResolvedValueOnce({ plan_id: 'p-abc123' });

    await expect(invokeDeletePlan('p-abc123')).rejects.toThrow(/^delete_plan/);
  });
});

describe('invokeSetTaskAssignment', () => {
  it('calls set_task_assignment with plan_id, task_id and the assignment', async () => {
    invokeMock.mockResolvedValueOnce(planInfoWire());
    const assignment: TaskAssignment = { profile: 'codex-cli', model: null, effort: null };

    await invokeSetTaskAssignment('p-abc123', 'investigate', assignment);

    expect(invokeMock).toHaveBeenCalledWith('set_task_assignment', {
      plan_id: 'p-abc123',
      task_id: 'investigate',
      assignment,
    });
  });

  it('narrows the reply into a PlanInfo', async () => {
    invokeMock.mockResolvedValueOnce(planInfoWire({ title: 'After override' }));

    const plan = await invokeSetTaskAssignment('p-abc123', 'investigate', {
      profile: 'codex-cli',
      model: null,
      effort: null,
    });

    expect(plan.title).toBe('After override');
  });
});

describe('invokeClearTaskAssignment', () => {
  it('calls clear_task_assignment with plan_id and task_id', async () => {
    invokeMock.mockResolvedValueOnce(planInfoWire());

    await invokeClearTaskAssignment('p-abc123', 'investigate');

    expect(invokeMock).toHaveBeenCalledWith('clear_task_assignment', {
      plan_id: 'p-abc123',
      task_id: 'investigate',
    });
  });

  it('narrows the reply into a PlanInfo', async () => {
    invokeMock.mockResolvedValueOnce(planInfoWire({ title: 'After clear' }));

    const plan = await invokeClearTaskAssignment('p-abc123', 'investigate');

    expect(plan.title).toBe('After clear');
  });
});

describe('invokeSetPlanOwner', () => {
  it('calls set_plan_owner with plan_id and window_id', async () => {
    invokeMock.mockResolvedValueOnce(planInfoWire());

    await invokeSetPlanOwner('p-abc123', 'window-b');

    expect(invokeMock).toHaveBeenCalledWith('set_plan_owner', {
      plan_id: 'p-abc123',
      window_id: 'window-b',
    });
  });

  it('narrows the reply into a PlanInfo', async () => {
    invokeMock.mockResolvedValueOnce(planInfoWire({ owner_window: 'window-b' }));

    const plan = await invokeSetPlanOwner('p-abc123', 'window-b');

    expect(plan.owner_window).toBe('window-b');
  });
});

describe('invokeListRoles', () => {
  it('calls list_roles with no arguments', async () => {
    invokeMock.mockResolvedValueOnce({ roles: [roleEntryWire()] });

    await invokeListRoles();

    expect(invokeMock).toHaveBeenCalledWith('list_roles');
  });

  it('narrows the reply into the role rows', async () => {
    invokeMock.mockResolvedValueOnce({ roles: [roleEntryWire({ name: 'implement' })] });

    const roles: RoleEntry[] = (await invokeListRoles()).roles;

    expect(roles).toHaveLength(1);
    expect(roles[0]?.name).toBe('implement');
  });

  it('rejects with BoundaryError when the reply fails validation', async () => {
    invokeMock.mockResolvedValueOnce({ roles: [roleEntryWire({ name: 7 })] });

    await expect(invokeListRoles()).rejects.toThrow(BoundaryError);
  });
});

describe('invokeSaveRoles', () => {
  it('calls save_roles with the roles array', async () => {
    const roles: RoleEntry[] = [
      { name: 'review', profile: 'codex-cli', model: null, effort: null },
    ];
    invokeMock.mockResolvedValueOnce({ roles });

    await invokeSaveRoles(roles, null);

    expect(invokeMock).toHaveBeenCalledWith('save_roles', { roles, default_role: null });
  });

  it('narrows the reply into the stored role rows', async () => {
    invokeMock.mockResolvedValueOnce({ roles: [roleEntryWire({ effort: 'high' })] });

    const stored = await invokeSaveRoles(
      [{ name: 'review', profile: 'codex-cli', model: null, effort: null }],
      null
    );

    expect(stored.roles[0]?.effort).toBe('high');
  });
});

describe('invokeApprovePlan', () => {
  it('calls approve_plan with the plan id and the version the caller read', async () => {
    invokeMock.mockResolvedValueOnce(planInfoWire({ review: 'approved', version: 4 }));

    await invokeApprovePlan('p-abc123', 4);

    expect(invokeMock).toHaveBeenCalledWith('approve_plan', { plan_id: 'p-abc123', version: 4 });
  });

  it('narrows the reply into a PlanInfo carrying its review state', async () => {
    invokeMock.mockResolvedValueOnce(planInfoWire({ review: 'approved', version: 4 }));

    const plan: PlanInfo = await invokeApprovePlan('p-abc123', 4);

    expect(plan.review).toBe('approved');
    expect(plan.version).toBe(4);
  });

  it('rejects with BoundaryError when the reply fails validation', async () => {
    invokeMock.mockResolvedValueOnce(planInfoWire({ review: 'bogus' }));

    await expect(invokeApprovePlan('p-abc123', 4)).rejects.toThrow(BoundaryError);
  });
});

describe('invokeMarkPlanSeen', () => {
  it('calls mark_plan_seen with the plan id and the version the caller read', async () => {
    invokeMock.mockResolvedValueOnce(planInfoWire({ review: 'approved', version: 9 }));

    await invokeMarkPlanSeen('p-abc123', 9);

    expect(invokeMock).toHaveBeenCalledWith('mark_plan_seen', { plan_id: 'p-abc123', version: 9 });
  });

  it('narrows the reply into a PlanInfo', async () => {
    invokeMock.mockResolvedValueOnce(planInfoWire({ review: 'approved', version: 9 }));

    const plan: PlanInfo = await invokeMarkPlanSeen('p-abc123', 9);

    expect(plan.version).toBe(9);
    expect(plan.tasks[0]?.review_mark).toBeNull();
  });

  it('rejects with BoundaryError when the reply fails validation', async () => {
    invokeMock.mockResolvedValueOnce(planInfoWire({ version: -1 }));

    await expect(invokeMarkPlanSeen('p-abc123', 9)).rejects.toThrow(BoundaryError);
  });
});

describe('the default role through the role commands', () => {
  it('reads the default role list_roles replies with', async () => {
    invokeMock.mockResolvedValueOnce({ roles: [roleEntryWire()], default_role: 'review' });

    const table: RoleTable = await invokeListRoles();

    expect(table.default_role).toBe('review');
    expect(table.roles.map((role) => role.name)).toEqual(['review']);
  });

  it('passes the chosen default role to save_roles', async () => {
    const roles: RoleEntry[] = [
      { name: 'review', profile: 'codex-cli', model: null, effort: null },
    ];
    invokeMock.mockResolvedValueOnce({ roles, default_role: 'review' });

    await invokeSaveRoles(roles, 'review');

    expect(invokeMock).toHaveBeenCalledWith('save_roles', { roles, default_role: 'review' });
  });

  it('returns the default role as the daemon stored it', async () => {
    invokeMock.mockResolvedValueOnce({ roles: [roleEntryWire()] });

    const stored: RoleTable = await invokeSaveRoles(
      [{ name: 'review', profile: 'codex-cli', model: null, effort: null }],
      'review'
    );

    expect(stored.default_role).toBeNull();
  });

  it('rejects with BoundaryError when the default role fails validation', async () => {
    invokeMock.mockResolvedValueOnce({ roles: [roleEntryWire()], default_role: 3 });

    await expect(invokeListRoles()).rejects.toThrow(BoundaryError);
  });
});

describe('subscribePlanChanged', () => {
  it('subscribes to session://plan-changed', () => {
    expect(SESSION_EVENT.planChanged).toBe('session://plan-changed');
  });

  it('delivers a well-formed payload to onPlanChanged', async () => {
    let registered: ListenCallback | undefined;
    listenMock.mockImplementation((name: string, callback: ListenCallback) => {
      expect(name).toBe(SESSION_EVENT.planChanged);
      registered = callback;
      return Promise.resolve(unlistenMock);
    });
    const onPlanChanged = vi.fn();
    const onBoundaryError = vi.fn();

    await subscribePlanChanged(onPlanChanged, onBoundaryError);
    registered?.({ payload: { plan_id: 'p-abc123' } });

    expect(onPlanChanged).toHaveBeenCalledWith({ plan_id: 'p-abc123' });
    expect(onBoundaryError).toHaveBeenCalledTimes(0);
  });

  it('reports a malformed payload through onBoundaryError instead of throwing', async () => {
    let registered: ListenCallback | undefined;
    listenMock.mockImplementation((_name: string, callback: ListenCallback) => {
      registered = callback;
      return Promise.resolve(unlistenMock);
    });
    const onPlanChanged = vi.fn();
    const onBoundaryError = vi.fn();

    await subscribePlanChanged(onPlanChanged, onBoundaryError);

    expect(() => registered?.({ payload: {} })).not.toThrow();
    expect(onPlanChanged).toHaveBeenCalledTimes(0);
    expect(onBoundaryError).toHaveBeenCalledTimes(1);
  });

  it('returns a function that cancels the subscription', async () => {
    listenMock.mockResolvedValueOnce(unlistenMock);

    const cancel = await subscribePlanChanged(vi.fn(), vi.fn());
    cancel();

    expect(unlistenMock).toHaveBeenCalledTimes(1);
  });
});
