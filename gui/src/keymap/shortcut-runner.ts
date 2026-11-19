/**
 * What each shortcut action does once the dispatcher has matched it. Window
 * operations go through the same actions the title bar and the sidebar use,
 * so confirmation and the work mode's handling live in one place.
 */

import { neighborWindow, type WorkMode } from '../session/current-window';
import { revealWindow, type RevealDeps, type RoomMember } from '../session/room-navigation';
import { exitedCardNeighbor, type ExitedCard } from '../session/shell-state';
import { windowsInDisplayOrder, type RoomGroup } from '../sidebar/room-groups';
import type { ActionId } from './key-binding';

/** The window operations a shortcut can start. */
export interface ShortcutWindowActions<T> {
  close: (entry: T) => Promise<void>;
  interrupt: (entry: T) => Promise<void>;
  minimize: (entry: T) => void;
  maximize: (entry: T) => void;
  /** Brings a window on screen in the current work mode. */
  goTo: (entry: T) => void;
}

/** Which of its two modes the picker opens in: windows by name, or actions after `>`. */
export type PickerMode = 'windows' | 'commands';

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
  toggleSidebarFocus: () => void;
}

/**
 * Builds the function that runs a shortcut action. Operations on "the current
 * window" act on the injected one and do nothing without it; minimize, maximize
 * and tile do nothing in Focus, which has none of them. Next / previous walk the
 * sidebar order and wrap at both ends; while an ended window's card is shown
 * they go where the card's rule says instead, so the key and its button agree.
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

  function neighborOf(step: 1 | -1): T | null {
    const card = deps.exitedCard();
    if (card === null) {
      return neighborWindow(deps.displayOrder(), deps.current(), step);
    }
    const targetId = exitedCardNeighbor(card, step);
    return (
      windowsInDisplayOrder(deps.groups()).find((entry) => entry.windowId === targetId) ?? null
    );
  }

  function goToNeighbor(step: 1 | -1): void {
    const target = neighborOf(step);
    if (target !== null) {
      revealWindow(reveal, target);
    }
  }

  async function onCurrent(operate: (entry: T) => void | Promise<void>): Promise<void> {
    const target = deps.current();
    if (target !== null) {
      await operate(target);
    }
  }

  return async (action: ActionId): Promise<void> => {
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
      case 'close-window':
        await onCurrent(deps.actions.close);
        return;
      case 'interrupt-window':
        await onCurrent(deps.actions.interrupt);
        return;
      case 'rename-window':
        await onCurrent(deps.renameWindow);
        return;
      case 'minimize-window':
        if (deps.workMode() === 'canvas') {
          await onCurrent(deps.actions.minimize);
        }
        return;
      case 'toggle-maximize-window':
        if (deps.workMode() === 'canvas') {
          await onCurrent(deps.actions.maximize);
        }
        return;
      case 'toggle-work-mode':
        deps.toggleWorkMode();
        return;
      case 'tile-windows':
        if (deps.workMode() === 'canvas') {
          deps.tileWindows();
        }
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
      default: {
        const unhandled: never = action;
        return unhandled;
      }
    }
  };
}
