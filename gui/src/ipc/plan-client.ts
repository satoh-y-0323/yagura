/**
 * Thin client for the plan-ledger commands of the Tauri core, and the
 * subscription to the plan-changed pushes it forwards from the daemon.
 *
 * Every value received from the core passes through `plan-boundary` before
 * it reaches a caller. A refusal from the core -- including one from a
 * daemon that predates the plan ledger -- rejects the returned promise
 * rather than resolving to an empty result.
 */

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import {
  narrowPlanChanged,
  narrowPlanInfo,
  narrowPlanList,
  narrowRoleList,
  type PlanChanged,
  type PlanInfo,
  type PlanSummary,
  type RoleEntry,
  type RoleTable,
  type TaskAssignment,
} from './plan-boundary';
import { narrowStringResult, SESSION_EVENT } from './session-boundary';

/**
 * Lists the plans the daemon lets this GUI see.
 *
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeListPlans(): Promise<PlanSummary[]> {
  return narrowPlanList(await invoke<unknown>('list_plans'));
}

/**
 * Reads one plan in full.
 *
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeGetPlan(planId: string): Promise<PlanInfo> {
  return narrowPlanInfo(await invoke<unknown>('get_plan', { plan_id: planId }));
}

/**
 * Deletes one plan.
 *
 * @returns the id of the plan the daemon deleted.
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeDeletePlan(planId: string): Promise<string> {
  return narrowStringResult(
    await invoke<unknown>('delete_plan', { plan_id: planId }),
    'delete_plan'
  );
}

/**
 * Sets the user's assignment override for one task.
 *
 * @returns the plan as stored afterwards.
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeSetTaskAssignment(
  planId: string,
  taskId: string,
  assignment: TaskAssignment
): Promise<PlanInfo> {
  return narrowPlanInfo(
    await invoke<unknown>('set_task_assignment', {
      plan_id: planId,
      task_id: taskId,
      assignment,
    })
  );
}

/**
 * Removes the user's assignment override from one task.
 *
 * @returns the plan as stored afterwards.
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeClearTaskAssignment(planId: string, taskId: string): Promise<PlanInfo> {
  return narrowPlanInfo(
    await invoke<unknown>('clear_task_assignment', { plan_id: planId, task_id: taskId })
  );
}

/**
 * Names a window as a plan's owner.
 *
 * @returns the plan as stored afterwards.
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeSetPlanOwner(planId: string, windowId: string): Promise<PlanInfo> {
  return narrowPlanInfo(
    await invoke<unknown>('set_plan_owner', { plan_id: planId, window_id: windowId })
  );
}

/**
 * Approves a plan awaiting review. `version` is the one the caller last read,
 * so the daemon refuses the approval if the tasks changed since.
 *
 * @returns the plan as stored afterwards.
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeApprovePlan(planId: string, version: number): Promise<PlanInfo> {
  return narrowPlanInfo(await invoke<unknown>('approve_plan', { plan_id: planId, version }));
}

/**
 * Tells the daemon the user has looked at a plan's marked tasks. `version` is
 * the one the caller last read, so marks that arrived after it stay.
 *
 * @returns the plan as stored afterwards.
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeMarkPlanSeen(planId: string, version: number): Promise<PlanInfo> {
  return narrowPlanInfo(await invoke<unknown>('mark_plan_seen', { plan_id: planId, version }));
}

/**
 * Reads the role-to-assignment table and its default role.
 *
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeListRoles(): Promise<RoleTable> {
  return narrowRoleList(await invoke<unknown>('list_roles'));
}

/**
 * Replaces the whole role-to-assignment table and its default role; `null`
 * clears the default.
 *
 * @returns the table as the daemon stored it.
 * @throws when the core rejects the table or the reply fails validation.
 */
export async function invokeSaveRoles(
  roles: RoleEntry[],
  defaultRole: string | null
): Promise<RoleTable> {
  return narrowRoleList(await invoke<unknown>('save_roles', { roles, default_role: defaultRole }));
}

/**
 * Subscribes to the plan-changed pushes the core forwards from the daemon.
 *
 * A payload that fails validation is reported and dropped, the same way the
 * chat-changed pushes are: an unusable push never disturbs what is shown.
 *
 * @returns a function that cancels the subscription.
 */
export async function subscribePlanChanged(
  onPlanChanged: (payload: PlanChanged) => void,
  onBoundaryError: (error: unknown) => void
): Promise<UnlistenFn> {
  return listen<unknown>(SESSION_EVENT.planChanged, (event) => {
    let payload: PlanChanged;
    try {
      payload = narrowPlanChanged(event.payload);
    } catch (error) {
      onBoundaryError(error);
      return;
    }
    onPlanChanged(payload);
  });
}
