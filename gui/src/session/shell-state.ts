/**
 * The work area's state and the decisions over it, as pure functions: the work
 * mode, the window Focus shows, and the window Focus starts on. Held in memory
 * only; the work mode alone is read from injected storage.
 */

import { STATUS_PRESENTATION } from '../status-presentation';
import { needsYou, type NeedsYouInput } from '../status-tone';
import type { WorkMode, WorkView } from './current-window';

const WORK_MODE_KEY = 'yagura.work-mode';

/** What the work area holds besides the canvas placement. */
export interface ShellState {
  workMode: WorkMode;
  /** The window Focus shows; ignored on the canvas, which reads the placement. */
  focusedWindowId: string | null;
}

/** What the user did to the work area. */
export type ShellEvent =
  /** Chooses the window Focus shows. */
  | { type: 'select'; windowId: string }
  /** Switches from Canvas to Focus, which starts on the canvas's current window. */
  | { type: 'enterFocus'; canvasCurrent: { windowId: string } | null }
  /** Switches from Focus to Canvas. */
  | { type: 'enterCanvas' };

/** What the view carries out after a step; the reducer touches no placement itself. */
export type ShellEffect = { type: 'bringToFront'; windowId: string };

/** The state after one event, and the effects it asks for. */
export interface ShellStep {
  state: ShellState;
  effects: readonly ShellEffect[];
}

/** The state before any window has been chosen. */
export function initialShellState(workMode: WorkMode): ShellState {
  return { workMode, focusedWindowId: null };
}

/**
 * Applies one event. Choosing a window in Focus and entering Focus never ask
 * for a placement change, so the saved layout stays as the canvas left it;
 * leaving Focus asks to bring its window to the front of the canvas.
 */
export function reduceShell(state: ShellState, event: ShellEvent): ShellStep {
  switch (event.type) {
    case 'select':
      return { state: { ...state, focusedWindowId: event.windowId }, effects: [] };
    case 'enterFocus':
      if (state.workMode === 'focus') {
        return { state, effects: [] };
      }
      return {
        state: {
          ...state,
          workMode: 'focus',
          focusedWindowId: event.canvasCurrent?.windowId ?? null,
        },
        effects: [],
      };
    case 'enterCanvas': {
      if (state.workMode === 'canvas') {
        return { state, effects: [] };
      }
      const effects: ShellEffect[] =
        state.focusedWindowId === null
          ? []
          : [{ type: 'bringToFront', windowId: state.focusedWindowId }];
      return { state: { ...state, workMode: 'canvas' }, effects };
    }
  }
}

/** The view the current window is derived from (`currentWindowFor`, `current-window.ts`). */
export function workViewOf(state: ShellState): WorkView {
  return state.workMode === 'canvas'
    ? { mode: 'canvas' }
    : { mode: 'focus', focusedWindowId: state.focusedWindowId };
}

/** The saved work mode; a missing or unknown value is Focus, the default. */
export function readWorkMode(storage: { getItem: (key: string) => string | null }): WorkMode {
  return storage.getItem(WORK_MODE_KEY) === 'canvas' ? 'canvas' : 'focus';
}

/**
 * The window Focus shows when the app starts: the window needing the user that
 * ranks first (the sidebar order breaks a tie), else the canvas's current
 * window, else the top of the sidebar.
 *
 * @param order the windows in the order the sidebar renders them.
 * @param canvasCurrent the current window of the restored canvas placement.
 * @returns null when there is no window.
 */
export function firstWindowAtStartup<T extends NeedsYouInput>(
  order: readonly T[],
  canvasCurrent: T | null
): T | null {
  let picked: T | null = null;
  for (const entry of order) {
    if (!needsYou(entry)) {
      continue;
    }
    if (
      picked === null ||
      STATUS_PRESENTATION[entry.detected.status].rank <
        STATUS_PRESENTATION[picked.detected.status].rank
    ) {
      picked = entry;
    }
  }
  return picked ?? canvasCurrent ?? order[0] ?? null;
}
