/**
 * The work area's state and the decisions over it, as pure functions: the work
 * mode, the window Focus shows, the window Focus starts on, and what follows
 * when a window leaves or joins the list. Held in memory only; the work mode
 * alone is read from injected storage.
 */

import type { DisplayLine } from '../display/untrusted-text';
import { STATUS_PRESENTATION } from '../status-presentation';
import { needsYou, type NeedsYouInput } from '../status-tone';
import type { WorkMode, WorkView } from './current-window';

const WORK_MODE_KEY = 'yagura.work-mode';

/** A window as the sidebar listed it, in the sidebar's order. */
export interface OrderedWindow {
  windowId: string;
  /** The name as the sidebar shows it. */
  name: DisplayLine;
  exitCode: number | null;
}

/** The card Focus shows in place of a window that ended without this GUI closing it. */
export interface ExitedCard {
  windowId: string;
  name: DisplayLine;
  /** `null` when the code could not be determined. */
  exitCode: number | null;
  /** The name of the window the card offers to go to, which Next window also goes to. */
  next: DisplayLine | null;
  nextWindowId: string | null;
  /** The window Previous window goes to while the card is shown. */
  previousWindowId: string | null;
}

/** What the work area holds besides the canvas placement. */
export interface ShellState {
  workMode: WorkMode;
  /** The window Focus shows; ignored on the canvas, which reads the placement. */
  focusedWindowId: string | null;
  /** The windows this GUI asked the daemon to close that have not left the list yet. */
  closingHere: ReadonlySet<string>;
  exitedCard: ExitedCard | null;
}

/** One window that left the list, with the order it is read against. */
export interface WindowGoneEvent {
  type: 'windowGone';
  windowId: string;
  /** Its name as the sidebar showed it. */
  name: DisplayLine;
  /** The sidebar order before it left, holding it and the windows still listed. */
  orderBefore: readonly OrderedWindow[];
  /** The code the window ended with, `null` when the code could not be determined. */
  exitCode: number | null;
}

/** What happened to the work area: the user's choices and the window list's changes. */
export type ShellEvent =
  /** Chooses the window Focus shows. */
  | { type: 'select'; windowId: string }
  /** Switches from Canvas to Focus, which starts on the canvas's current window. */
  | { type: 'enterFocus'; canvasCurrent: { windowId: string } | null }
  /** Switches from Focus to Canvas. */
  | { type: 'enterCanvas' }
  /** This GUI is about to ask the daemon to close the window. */
  | { type: 'closeRequested'; windowId: string }
  /** The close this GUI asked for did not go through. */
  | { type: 'closeFailed'; windowId: string }
  | WindowGoneEvent
  /** The user opened the window from this GUI. */
  | { type: 'openedByUser'; windowId: string }
  /** Windows appeared any other way: another client, the daemon, or a reopening. */
  | { type: 'windowsAppeared'; windowIds: readonly string[] }
  /** The user pressed the button on the card of an ended window. */
  | { type: 'exitedCardGoTo' };

/**
 * What the view carries out after a step; the reducer touches no placement
 * itself. Keyboard focus moves only through `focusTerminal`.
 */
export type ShellEffect =
  { type: 'bringToFront'; windowId: string } | { type: 'focusTerminal'; windowId: string };

/** The state after one event, and the effects it asks for. */
export interface ShellStep {
  state: ShellState;
  effects: readonly ShellEffect[];
  /** Set only for `windowGone`: whether this GUI closed the window. */
  goneByUser?: boolean;
}

/** The state before any window has been chosen. */
export function initialShellState(workMode: WorkMode): ShellState {
  return {
    workMode,
    focusedWindowId: null,
    closingHere: new Set(),
    exitedCard: null,
  };
}

/**
 * One `windowGone` for each window in `previous` that `current` no longer
 * lists. Each event's order leaves out the other windows that left with it, so
 * a next window read from it is one still listed.
 */
export function diffGoneWindows(
  previous: readonly OrderedWindow[],
  current: readonly OrderedWindow[]
): WindowGoneEvent[] {
  const listed = new Set(current.map((entry) => entry.windowId));
  const goneIds = new Set(
    previous.filter((entry) => !listed.has(entry.windowId)).map((entry) => entry.windowId)
  );
  return previous
    .filter((entry) => goneIds.has(entry.windowId))
    .map((entry) => ({
      type: 'windowGone',
      windowId: entry.windowId,
      name: entry.name,
      orderBefore: previous.filter(
        (other) => other.windowId === entry.windowId || !goneIds.has(other.windowId)
      ),
      exitCode: entry.exitCode,
    }));
}

/** The `windowsAppeared` event for the windows `current` lists and `previous` did not. */
export function diffAppearedWindows(
  previous: readonly OrderedWindow[],
  current: readonly OrderedWindow[]
): ShellEvent[] {
  const known = new Set(previous.map((entry) => entry.windowId));
  const windowIds = current
    .filter((entry) => !known.has(entry.windowId))
    .map((entry) => entry.windowId);
  return windowIds.length === 0 ? [] : [{ type: 'windowsAppeared', windowIds }];
}

/**
 * The window one `step` from `windowId` in `order`, else the one on the other
 * side; never wraps around, so an end does not jump to the far end.
 */
function adjacentWindow(
  order: readonly OrderedWindow[],
  windowId: string,
  step: 1 | -1
): OrderedWindow | null {
  const index = order.findIndex((entry) => entry.windowId === windowId);
  if (index === -1) {
    return null;
  }
  return order[index + step] ?? order[index - step] ?? null;
}

function withoutMember(set: ReadonlySet<string>, member: string): ReadonlySet<string> {
  if (!set.has(member)) {
    return set;
  }
  const next = new Set(set);
  next.delete(member);
  return next;
}

/** Keeps the card's targets off a window that has since left the list as well. */
function retargetCard(card: ExitedCard | null, event: WindowGoneEvent): ExitedCard | null {
  if (card === null) {
    return null;
  }
  let retargeted = card;
  if (card.nextWindowId === event.windowId) {
    const next = adjacentWindow(event.orderBefore, event.windowId, 1);
    retargeted = { ...retargeted, next: next?.name ?? null, nextWindowId: next?.windowId ?? null };
  }
  if (card.previousWindowId === event.windowId) {
    const previous = adjacentWindow(event.orderBefore, event.windowId, -1);
    retargeted = { ...retargeted, previousWindowId: previous?.windowId ?? null };
  }
  return retargeted;
}

/**
 * Where Next window (`step` 1) or Previous window (`step` -1) goes while the
 * card is shown: from where the ended window stood, by the rule its button uses.
 *
 * @returns null when no window is left to go to.
 */
export function exitedCardNeighbor(card: ExitedCard, step: 1 | -1): string | null {
  return step === 1 ? card.nextWindowId : card.previousWindowId;
}

/**
 * The one place that decides who closed a window: this GUI did when it asked
 * to before the window left the list, whichever news removed it first. Only a
 * window this GUI closed moves Focus and the keyboard on; one that ended
 * otherwise leaves a card, so keys being typed never reach another window.
 */
function reduceWindowGone(state: ShellState, event: WindowGoneEvent): ShellStep {
  const goneByUser = state.closingHere.has(event.windowId);
  const base: ShellState = {
    ...state,
    closingHere: withoutMember(state.closingHere, event.windowId),
    exitedCard: retargetCard(state.exitedCard, event),
  };
  if (state.workMode !== 'focus' || state.focusedWindowId !== event.windowId) {
    return { state: base, effects: [], goneByUser };
  }
  const next = adjacentWindow(event.orderBefore, event.windowId, 1);
  if (goneByUser) {
    return {
      state: { ...base, focusedWindowId: next?.windowId ?? null, exitedCard: null },
      effects: next === null ? [] : [{ type: 'focusTerminal', windowId: next.windowId }],
      goneByUser,
    };
  }
  return {
    state: {
      ...base,
      exitedCard: {
        windowId: event.windowId,
        name: event.name,
        exitCode: event.exitCode,
        next: next?.name ?? null,
        nextWindowId: next?.windowId ?? null,
        previousWindowId: adjacentWindow(event.orderBefore, event.windowId, -1)?.windowId ?? null,
      },
    },
    effects: [],
    goneByUser,
  };
}

/**
 * Applies one event. Choosing a window in Focus and entering Focus never ask
 * for a placement change, so the saved layout stays as the canvas left it;
 * leaving Focus asks to bring its window to the front of the canvas. Only the
 * user's own acts ask to move the keyboard.
 */
export function reduceShell(state: ShellState, event: ShellEvent): ShellStep {
  switch (event.type) {
    case 'select':
      return {
        state: { ...state, focusedWindowId: event.windowId, exitedCard: null },
        effects: [],
      };
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
      // A carded window has left the list, so there is nothing to bring forward.
      const shown = state.exitedCard === null ? state.focusedWindowId : null;
      const effects: ShellEffect[] =
        shown === null ? [] : [{ type: 'bringToFront', windowId: shown }];
      return { state: { ...state, workMode: 'canvas', exitedCard: null }, effects };
    }
    case 'closeRequested':
      return {
        state: { ...state, closingHere: new Set(state.closingHere).add(event.windowId) },
        effects: [],
      };
    case 'closeFailed':
      return {
        state: { ...state, closingHere: withoutMember(state.closingHere, event.windowId) },
        effects: [],
      };
    case 'windowGone':
      return reduceWindowGone(state, event);
    case 'openedByUser':
      if (state.workMode === 'canvas') {
        return {
          state,
          effects: [
            { type: 'bringToFront', windowId: event.windowId },
            { type: 'focusTerminal', windowId: event.windowId },
          ],
        };
      }
      return {
        state: { ...state, focusedWindowId: event.windowId, exitedCard: null },
        effects: [{ type: 'focusTerminal', windowId: event.windowId }],
      };
    case 'windowsAppeared': {
      const first = event.windowIds[0];
      if (state.workMode !== 'focus' || state.focusedWindowId !== null || first === undefined) {
        return { state, effects: [] };
      }
      return { state: { ...state, focusedWindowId: first }, effects: [] };
    }
    case 'exitedCardGoTo': {
      const target = state.exitedCard?.nextWindowId ?? null;
      if (target === null) {
        return { state, effects: [] };
      }
      return {
        state: { ...state, focusedWindowId: target, exitedCard: null },
        effects: [{ type: 'focusTerminal', windowId: target }],
      };
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
 * Saves the work mode where {@link readWorkMode} reads it. Does not guard
 * against a failing `setItem`; the caller passes a guarded storage.
 */
export function writeWorkMode(storage: Pick<Storage, 'setItem'>, mode: WorkMode): void {
  storage.setItem(WORK_MODE_KEY, mode);
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
