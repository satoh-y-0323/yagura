/**
 * Whether an action does anything right now, and why not when it does not.
 * The shortcut runner, the command palette and the Navigator's menu all read
 * this one table, so a key, a palette row and a menu item never disagree.
 */

import type { WindowStatus } from '../ipc/session-boundary';
import type { WorkMode } from '../session/current-window';
import { resolveConfirmTarget, type InterruptBlockReason } from '../session/confirm-target';
import type { ActionId } from './key-binding';

/** The actions that operate on the current window. */
export const WINDOW_ACTION_IDS = [
  'close-window',
  'interrupt-window',
  'minimize-window',
  'toggle-maximize-window',
  'rename-window',
] as const satisfies readonly ActionId[];

/** An action that operates on the current window. */
export type WindowActionId = (typeof WINDOW_ACTION_IDS)[number];

/** Whether `action` operates on the current window. */
export function isWindowAction(action: ActionId): action is WindowActionId {
  return (WINDOW_ACTION_IDS as readonly ActionId[]).includes(action);
}

/** What the judgment reads: the state an action would act on, taken at one moment. */
export interface AvailabilityContext<T> {
  /** The current window of the work view and its status, or null when none is on screen. */
  current: { entry: T; status: WindowStatus } | null;
  workMode: WorkMode;
  /** Whether next / previous have a window to go to, walking as the runner walks. */
  hasNeighbor: { next: boolean; previous: boolean };
  /** Whether a window needing the user is there to jump to. */
  hasAttentionTarget: boolean;
  /** How many windows are open, whatever the Needs you filter shows. */
  windowCount: number;
}

/** Why an action does nothing right now. */
export type ActionBlockReason =
  | 'no-window-on-screen'
  | 'canvas-only'
  | 'no-windows'
  | 'nowhere-to-go'
  | 'nothing-needs-you'
  | InterruptBlockReason;

/** Whether an action does anything right now. */
export type ActionVerdict = { ok: true } | { ok: false; reason: ActionBlockReason };

/** Whether a window action does anything right now, and the window it acts on when it does. */
export type WindowActionVerdict<T> =
  { ok: true; window: T } | { ok: false; reason: ActionBlockReason };

const ALLOWED: ActionVerdict = { ok: true };

function refused(reason: ActionBlockReason): { ok: false; reason: ActionBlockReason } {
  return { ok: false, reason };
}

/**
 * Judges a window action. The window comes back on the allowed arm, so the
 * caller acts on it without testing for "no current window" a second time.
 */
export function windowActionAvailability<T>(
  action: WindowActionId,
  context: AvailabilityContext<T>
): WindowActionVerdict<T> {
  if (
    (action === 'minimize-window' || action === 'toggle-maximize-window') &&
    context.workMode !== 'canvas'
  ) {
    return refused('canvas-only');
  }
  const current = context.current;
  if (current === null) {
    return refused('no-window-on-screen');
  }
  if (action === 'interrupt-window') {
    const target = resolveConfirmTarget(current.status, 'interrupt');
    if (!target.allowed) {
      return refused(target.reason);
    }
  }
  return { ok: true, window: current.entry };
}

/** Judges any action; a window action is judged by {@link windowActionAvailability}. */
export function actionAvailability<T>(
  action: ActionId,
  context: AvailabilityContext<T>
): ActionVerdict {
  if (isWindowAction(action)) {
    const verdict = windowActionAvailability(action, context);
    return verdict.ok ? ALLOWED : refused(verdict.reason);
  }
  switch (action) {
    case 'tile-windows':
      if (context.workMode !== 'canvas') {
        return refused('canvas-only');
      }
      return context.windowCount === 0 ? refused('no-windows') : ALLOWED;
    case 'focus-next-window':
      return context.hasNeighbor.next ? ALLOWED : refused('nowhere-to-go');
    case 'focus-previous-window':
      return context.hasNeighbor.previous ? ALLOWED : refused('nowhere-to-go');
    case 'focus-next-attention':
      return context.hasAttentionTarget ? ALLOWED : refused('nothing-needs-you');
    default:
      return ALLOWED;
  }
}
