/**
 * The single execution path for the window operations with a contract:
 * close / interrupt / rename / focus / minimize / maximize.
 * Move/resize are excluded on purpose: `FloatWindow` writes those
 * coordinate updates to the registry directly, without confirmation.
 *
 * The registry, ipc client and confirm port are injected at construction
 * time rather than imported, so this module has no
 * module-level singleton and stays usable with an independent registry
 * instance per test (or per application root).
 */

import type { CloseOutcome } from '../ipc/session-boundary';
import type { WindowEntry } from '../stores/window-registry';
import { resolveConfirmTarget, type WindowOperation } from './confirm-target';

/** The subset of {@link WindowRegistry} this module needs. */
export interface WindowActionsRegistry {
  renameWindow: (windowId: string, name: string) => void;
  bringWindowToFront: (windowId: string) => void;
  setWindowMinimized: (windowId: string, minimized: boolean) => void;
  setWindowMaximized: (windowId: string, maximized: boolean) => void;
}

/** The subset of the session IPC client this module needs. */
export interface WindowActionsIpcClient {
  invokeCloseWindow: (windowId: string) => Promise<CloseOutcome>;
  invokeInterruptWindow: (windowId: string) => Promise<void>;
  invokeRenameWindow: (windowId: string, name: string) => Promise<string>;
}

/**
 * Asks the user to approve a destructive operation.
 *
 * @returns whether the operation was approved.
 */
export type ConfirmPort = (operation: WindowOperation, entry: WindowEntry) => Promise<boolean>;

/** Dependencies of {@link createWindowActions}. */
export interface WindowActionsDeps {
  registry: WindowActionsRegistry;
  ipc: WindowActionsIpcClient;
  /**
   * Provided by `App.svelte` only. Absent for any other caller, in which
   * case destructive operations (close/interrupt) fail closed instead of
   * running unconfirmed.
   */
  confirmPort?: ConfirmPort;
  /**
   * Drops one window from the view. Called only after `invokeCloseWindow`
   * resolves, so a refused close never removes a live window: the IPC response
   * itself is the signal, as it is for `rename`. All three outcomes resolve --
   * `'not_found'` and `'unreachable'` are presentation cleanup, not a close
   * the daemon carried out.
   *
   * A failure here is not a failed close: close absorbs it (silently, no
   * `console` output) no matter how the provider implements it, so a
   * pane-removal error never surfaces as a refused close.
   */
  removeWindow?: (windowId: string) => void;
  /**
   * Whether the app is currently in tab mode. Provided by
   * `App.svelte` only; when absent, `focus`/`minimize` keep their float-window
   * behaviour unconditionally, the same fail-safe pattern `confirmPort` uses.
   */
  isTabMode?: () => boolean;
  /**
   * Switches the visible tab to `windowId` — the 3-connect
   * (`setWindowMinimized` -> `setWindowMaximized` -> `bringWindowToFront`).
   * Provided by `App.svelte` only.
   */
  switchToTab?: (windowId: string) => void;
  /**
   * Resolves the tab that should become active once `windowId` is
   * minimized, using the tab order; `null` when none
   * remains. Provided by `App.svelte` only; `minimize` only consults it once
   * it already knows it is in tab mode with a `switchToTab` to hand the
   * successor to.
   */
  resolveTabSuccessor?: (windowId: string) => string | null;
}

/** The window operations exposed to the view. */
export interface WindowActions {
  close: (entry: WindowEntry) => Promise<void>;
  interrupt: (entry: WindowEntry) => Promise<void>;
  rename: (entry: WindowEntry, name: string) => Promise<void>;
  focus: (entry: WindowEntry) => void;
  minimize: (entry: WindowEntry) => void;
  maximize: (entry: WindowEntry) => void;
}

/** Builds a {@link WindowActions} bound to `deps`. */
export function createWindowActions(deps: WindowActionsDeps): WindowActions {
  const { registry, ipc, confirmPort, removeWindow, isTabMode, switchToTab, resolveTabSuccessor } =
    deps;

  /**
   * Runs a destructive operation through `resolveConfirmTarget`'s decision:
   * not allowed -> skip, allowed without confirmation -> run, allowed with
   * confirmation -> ask the confirm port first, failing closed when none is
   * registered.
   */
  const runDestructive = async (
    entry: WindowEntry,
    operation: WindowOperation,
    execute: () => Promise<void>
  ): Promise<void> => {
    const target = resolveConfirmTarget(entry.detected.status, operation);
    if (!target.allowed) {
      return;
    }
    if (target.requiresConfirmation) {
      if (confirmPort === undefined) {
        return;
      }
      const approved = await confirmPort(operation, entry);
      if (!approved) {
        return;
      }
    }
    await execute();
  };

  return {
    close: (entry) =>
      runDestructive(entry, 'close', async () => {
        await ipc.invokeCloseWindow(entry.windowId);
        // A refused close (above) must still reject; a pane-removal failure
        // (here) must not, so it is caught separately rather than letting
        // both share the same try/catch.
        try {
          removeWindow?.(entry.windowId);
        } catch {
          // Absorbed: see the `removeWindow` doc on WindowActionsDeps.
        }
      }),
    // Interrupt has no refresh counterpart on purpose: what it changes is the
    // window's status, and a status transition reaches the view through its
    // own pushed event, not through the window list.
    interrupt: (entry) =>
      runDestructive(entry, 'interrupt', () => ipc.invokeInterruptWindow(entry.windowId)),
    rename: async (entry, name) => {
      const stored = await ipc.invokeRenameWindow(entry.windowId, name);
      registry.renameWindow(entry.windowId, stored);
    },
    focus: (entry) => {
      // In tab mode the same intent ("show me window X") switches tabs instead
      // of raising a float window. Decided before the
      // unminimize guard below because `switchToTab` restores the window itself,
      // and running both would dispatch that mutation twice.
      if ((isTabMode?.() ?? false) && switchToTab !== undefined) {
        switchToTab(entry.windowId);
        return;
      }
      if (entry.display.isMinimized) {
        registry.setWindowMinimized(entry.windowId, false);
      }
      registry.bringWindowToFront(entry.windowId);
    },
    minimize: (entry) => {
      registry.setWindowMinimized(entry.windowId, true);
      // Minimizing the active tab hands the strip to its successor, or ends tab
      // mode when none remains. `isMaximized` gates the branch
      // because only the active tab owns the strip.
      if (
        entry.display.isMaximized &&
        (isTabMode?.() ?? false) &&
        switchToTab !== undefined &&
        resolveTabSuccessor !== undefined
      ) {
        const successor = resolveTabSuccessor(entry.windowId);
        if (successor === null) {
          registry.setWindowMaximized(entry.windowId, false);
        } else {
          switchToTab(successor);
        }
      }
    },
    maximize: (entry) => {
      registry.setWindowMaximized(entry.windowId, !entry.display.isMaximized);
    },
  };
}
