/**
 * What each shortcut action does once the dispatcher has matched it. Window
 * operations go through the same actions the title bar and the sidebar use,
 * so confirmation and tab-mode handling live in one place.
 */

import { currentWindow, neighborWindow, type PlacedWindow } from '../session/current-window';
import { revealWindow, type RevealDeps } from '../session/room-navigation';
import type { RoomGroup } from '../sidebar/room-groups';
import type { ActionId } from './key-binding';

/** The window operations a shortcut can start. */
export interface ShortcutWindowActions<T> {
  close: (entry: T) => Promise<void>;
  interrupt: (entry: T) => Promise<void>;
  minimize: (entry: T) => void;
  maximize: (entry: T) => void;
  /** Brings a window to the front, un-minimizing or switching tabs as needed. */
  focus: (entry: T) => void;
}

/** What the runner reads and acts through; the view injects its own functions. */
export interface ShortcutRunnerDeps<T extends PlacedWindow> {
  /** Every open window. */
  windows: () => readonly T[];
  /** The windows in the order the sidebar renders them, top to bottom. */
  displayOrder: () => readonly T[];
  /** The sidebar's rooms as they are currently rendered, top to bottom. */
  groups: () => readonly RoomGroup<T>[];
  /** Opens a room without closing one that is already open. */
  expandRoom: (roomId: string) => void;
  actions: ShortcutWindowActions<T>;
  /** Brings the next window needing the user on screen; returns it, or null when none waits. */
  goToNextWindowNeedingYou: () => T | null;
  /** Asks for keyboard focus to move into a window's terminal. */
  focusTerminal: (windowId: string) => void;
  /** Returns keyboard focus to the current window, or to the sidebar when there is none. */
  restoreFocus: () => void;
  openWindowForm: () => void;
  toggleChat: () => Promise<void>;
  togglePlans: () => Promise<void>;
  toggleSettings: () => Promise<void>;
  toggleSidebarFocus: () => void;
}

/**
 * Builds the function that runs a shortcut action. Operations on "the current
 * window" act on {@link currentWindow} and do nothing when no window is on
 * screen; next / previous walk the sidebar order and wrap at both ends.
 *
 * @returns a function whose promise settles once the action, including any
 * confirmation it asked for, has finished.
 */
export function createShortcutRunner<T extends PlacedWindow>(
  deps: ShortcutRunnerDeps<T>
): (action: ActionId) => Promise<void> {
  const current = (): T | null => currentWindow(deps.windows());
  const reveal: RevealDeps<T> = {
    groups: deps.groups,
    expandRoom: deps.expandRoom,
    focus: deps.actions.focus,
    focusTerminal: deps.focusTerminal,
  };

  function goToNeighbor(step: 1 | -1): void {
    const target = neighborWindow(deps.displayOrder(), current(), step);
    if (target !== null) {
      revealWindow(reveal, target);
    }
  }

  async function closeCurrent(): Promise<void> {
    const target = current();
    if (target === null) {
      return;
    }
    await deps.actions.close(target);
    // A refused or cancelled close leaves the window, and focus, where they were.
    if (!deps.windows().some((entry) => entry.windowId === target.windowId)) {
      deps.restoreFocus();
    }
  }

  async function onCurrent(operate: (entry: T) => void | Promise<void>): Promise<void> {
    const target = current();
    if (target !== null) {
      await operate(target);
    }
  }

  return async (action: ActionId): Promise<void> => {
    switch (action) {
      case 'focus-next-window':
        goToNeighbor(1);
        return;
      case 'focus-previous-window':
        goToNeighbor(-1);
        return;
      case 'focus-next-attention':
        deps.goToNextWindowNeedingYou();
        return;
      case 'open-window-form':
        deps.openWindowForm();
        return;
      case 'close-window':
        await closeCurrent();
        return;
      case 'interrupt-window':
        await onCurrent(deps.actions.interrupt);
        return;
      case 'minimize-window':
        await onCurrent((entry) => {
          deps.actions.minimize(entry);
          deps.restoreFocus();
        });
        return;
      case 'toggle-maximize-window':
        await onCurrent(deps.actions.maximize);
        return;
      case 'toggle-sidebar-focus':
        deps.toggleSidebarFocus();
        return;
      case 'toggle-chat':
        await deps.toggleChat();
        return;
      case 'toggle-plans':
        await deps.togglePlans();
        return;
      case 'toggle-settings':
        await deps.toggleSettings();
        return;
      default: {
        const unhandled: never = action;
        return unhandled;
      }
    }
  };
}
