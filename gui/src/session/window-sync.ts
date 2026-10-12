/**
 * Applies a daemon window-list snapshot to the set of live panes.
 *
 * This is the step that makes every window of the daemon a pane of its own:
 * one snapshot in, one `openPane` per window that has no pane yet.
 */

import { planPanes, type PanePlan } from './pane-plan';

/**
 * What applying a snapshot needs from the view.
 *
 * The members are property signatures rather than methods so that callers can
 * pass them around as plain functions: they carry no `this`.
 */
export interface WindowSyncDeps {
  /** Creates the pane of one window; resolves once the pane exists. */
  openPane: (windowId: string) => Promise<void>;
  /** Tears the pane of one window down. */
  closePane: (windowId: string) => void;
  /** Reports that one window could not be opened or closed. */
  onError: (windowId: string, error: unknown) => void;
}

/**
 * Applies one window-list snapshot: opens missing panes and closes stale ones.
 *
 * Panes are opened one at a time, in daemon list order, so window connections
 * are established sequentially. A window that fails is reported
 * through `onError` and the remaining windows are still opened: one window failing
 * to attach must not leave the rest of the session unattached.
 *
 * Returns the plan it applied, so a caller can act on which ids opened or
 * closed without recomputing it.
 */
export async function syncWindows(
  daemonWindowIds: readonly string[],
  livePaneIds: readonly string[],
  deps: WindowSyncDeps
): Promise<PanePlan> {
  const plan = planPanes(daemonWindowIds, livePaneIds);
  for (const windowId of plan.toClose) {
    try {
      deps.closePane(windowId);
    } catch (error) {
      deps.onError(windowId, error);
    }
  }
  for (const windowId of plan.toOpen) {
    try {
      await deps.openPane(windowId);
    } catch (error) {
      deps.onError(windowId, error);
    }
  }
  return plan;
}
