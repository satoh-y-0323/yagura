/**
 * What each shortcut action does once the dispatcher has matched it. Window
 * operations go through the same actions the title bar and the sidebar use,
 * so confirmation and the work mode's handling live in one place.
 */

import type { WindowStatus } from '../ipc/session-boundary';
import { neighborWindow, type WorkMode } from '../session/current-window';
import { revealWindow, type RevealDeps, type RoomMember } from '../session/room-navigation';
import { exitedCardNeighbor, type ExitedCard } from '../session/shell-state';
import { windowsInDisplayOrder, type RoomGroup } from '../sidebar/room-groups';
import {
  actionAvailability,
  isWindowAction,
  windowActionAvailability,
  type AvailabilityContext,
  type WindowActionId,
} from './action-availability';
import type { ActionId } from './key-binding';
import type { PickerMode } from '../command-picker/picker-model';
export type { PickerMode };

/** The window operations a shortcut can start. */
export interface ShortcutWindowActions<T> {
  close: (entry: T) => Promise<void>;
  interrupt: (entry: T) => Promise<void>;
  minimize: (entry: T) => void;
  maximize: (entry: T) => void;
  /** Brings a window on screen in the current work mode. */
  goTo: (entry: T) => void;
}

/** What the runner reads and acts through; the view injects its own functions. */
export interface ShortcutRunnerDeps<T extends RoomMember> {
  /** The current window of the work view, or null when none is on screen. */
  current: () => T | null;
  workMode: () => WorkMode;
  /** The card standing in for the window Focus showed, or null while there is none. */
  exitedCard: () => ExitedCard | null;
  /** The windows in the order the sidebar renders them, top to bottom. */
  displayOrder: () => readonly T[];
  /** The sidebar's rooms as they are currently rendered, top to bottom. */
  groups: () => readonly RoomGroup<T>[];
  /** Opens a room without closing one that is already open. */
  expandRoom: (roomId: string) => void;
  actions: ShortcutWindowActions<T>;
  /** Brings the next window needing the user on screen; returns it, or null when none waits. */
  goToNextWindowNeedingYou: () => T | null;
  /** The window {@link goToNextWindowNeedingYou} would bring on screen now, without moving. */
  nextAttention: () => T | null;
  /** The detected status of `entry`. */
  statusOf: (entry: T) => WindowStatus;
  /** Asks for keyboard focus to move into a window's terminal. */
  focusTerminal: (windowId: string) => void;
  /** Opens the new-window dialog, raising its open flag within this call. */
  openWindowForm: () => void;
  /** Opens the rename dialog for `entry`, raising its open flag within this call. */
  renameWindow: (entry: T) => void;
  /** Opens the picker in `mode`. */
  openPicker: (mode: PickerMode) => void;
  /** Switches between Focus and Canvas. */
  toggleWorkMode: () => void;
  tileWindows: () => void;
  toggleNeedsYouFilter: () => void;
  toggleChat: () => Promise<void>;
  togglePlans: () => Promise<void>;
  toggleSettings: () => Promise<void>;
  toggleNotifications: () => void;
  toggleSidebarFocus: () => void;
}

/**
 * The window next / previous go to: along the sidebar order, wrapping at both
 * ends, or where an ended window's card says while one is shown, so the key
 * and the card's button agree.
 *
 * @returns the window, or null when there is none to go to
 */
export function neighborOf<T extends RoomMember>(
  deps: ShortcutRunnerDeps<T>,
  step: 1 | -1
): T | null {
  const card = deps.exitedCard();
  if (card === null) {
    return neighborWindow(deps.displayOrder(), deps.current(), step);
  }
  const targetId = exitedCardNeighbor(card, step);
  return windowsInDisplayOrder(deps.groups()).find((entry) => entry.windowId === targetId) ?? null;
}

/**
 * What {@link actionAvailability} judges, read from the same deps the runner
 * acts through, so the palette and the Navigator judge what the runner would do.
 */
export function availabilityContext<T extends RoomMember>(
  deps: ShortcutRunnerDeps<T>
): AvailabilityContext<T> {
  const entry = deps.current();
  return {
    current: entry === null ? null : { entry, status: deps.statusOf(entry) },
    workMode: deps.workMode(),
    hasNeighbor: { next: neighborOf(deps, 1) !== null, previous: neighborOf(deps, -1) !== null },
    hasAttentionTarget: deps.nextAttention() !== null,
    windowCount: windowsInDisplayOrder(deps.groups()).length,
  };
}

/**
 * Builds the function that runs a shortcut action. An action
 * {@link actionAvailability} refuses does nothing; a window action acts on
 * the window that judgment hands back.
 *
 * @returns a function whose promise settles once the action, including any
 * confirmation it asked for, has finished. Each action's deps are called before
 * the function returns, so a dialog an action opens is already marked open then.
 */
export function createShortcutRunner<T extends RoomMember>(
  deps: ShortcutRunnerDeps<T>
): (action: ActionId) => Promise<void> {
  const reveal: RevealDeps<T> = {
    groups: deps.groups,
    expandRoom: deps.expandRoom,
    goTo: deps.actions.goTo,
    focusTerminal: deps.focusTerminal,
  };

  function goToNeighbor(step: 1 | -1): void {
    const target = neighborOf(deps, step);
    if (target !== null) {
      revealWindow(reveal, target);
    }
  }

  async function runOnWindow(action: WindowActionId, window: T): Promise<void> {
    switch (action) {
      case 'close-window':
        await deps.actions.close(window);
        return;
      case 'interrupt-window':
        await deps.actions.interrupt(window);
        return;
      case 'rename-window':
        deps.renameWindow(window);
        return;
      case 'minimize-window':
        deps.actions.minimize(window);
        return;
      case 'toggle-maximize-window':
        deps.actions.maximize(window);
        return;
      default: {
        const unhandled: never = action;
        return unhandled;
      }
    }
  }

  return async (action: ActionId): Promise<void> => {
    const context = availabilityContext(deps);
    if (isWindowAction(action)) {
      const verdict = windowActionAvailability(action, context);
      if (verdict.ok) {
        await runOnWindow(action, verdict.window);
      }
      return;
    }
    if (!actionAvailability(action, context).ok) {
      return;
    }
    switch (action) {
      case 'open-window-picker':
        deps.openPicker('windows');
        return;
      case 'open-command-palette':
        deps.openPicker('commands');
        return;
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
      case 'toggle-work-mode':
        deps.toggleWorkMode();
        return;
      case 'tile-windows':
        deps.tileWindows();
        return;
      case 'toggle-sidebar-focus':
        deps.toggleSidebarFocus();
        return;
      case 'toggle-needs-you-filter':
        deps.toggleNeedsYouFilter();
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
      case 'toggle-notifications':
        deps.toggleNotifications();
        return;
      default: {
        const unhandled: never = action;
        return unhandled;
      }
    }
  };
}
