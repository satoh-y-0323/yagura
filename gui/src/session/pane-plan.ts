/**
 * Decides, from one daemon window-list snapshot, which panes have to be
 * created and which have to be torn down, and which entry/pane pairs are
 * ready to render.
 */

import type { SessionController } from './session-controller';
import type { SessionStore } from '../stores/session-store';
import type { WindowEntry } from '../stores/window-registry';

/** What drives one window's session and the state it shows. */
export interface Pane {
  controller: SessionController;
  sessionStore: SessionStore;
}

/** The panes one snapshot asks to be created and destroyed. */
export interface PanePlan {
  /** Window ids that need a pane/controller created, in daemon list order. */
  toOpen: string[];
  /** Window ids whose pane/controller must be torn down. */
  toClose: string[];
}

/**
 * Compares the daemon's window list against the panes that currently exist.
 *
 * Membership decides both lists, so applying the same snapshot twice yields an
 * empty plan. `toOpen` follows the daemon's order because panes are opened in
 * that order; `toClose` follows the order the panes are held in,
 * which teardown does not depend on.
 */
export function planPanes(
  daemonWindowIds: readonly string[],
  livePaneIds: readonly string[]
): PanePlan {
  const live = new Set(livePaneIds);
  const reported = new Set(daemonWindowIds);
  return {
    toOpen: daemonWindowIds.filter((windowId) => !live.has(windowId)),
    toClose: livePaneIds.filter((windowId) => !reported.has(windowId)),
  };
}

/**
 * Pairs each window-list entry with its pane, keeping only the ids both sides
 * agree on: an entry with no pane yet, or a pane whose window left the list,
 * is excluded rather than rendered with a missing half.
 */
export function pairPanes(
  entries: readonly WindowEntry[],
  panes: ReadonlyMap<string, Pane>
): Array<{ entry: WindowEntry; pane: Pane }> {
  const result: Array<{ entry: WindowEntry; pane: Pane }> = [];
  for (const entry of entries) {
    const pane = panes.get(entry.windowId);
    if (pane !== undefined) {
      result.push({ entry, pane });
    }
  }
  return result;
}
