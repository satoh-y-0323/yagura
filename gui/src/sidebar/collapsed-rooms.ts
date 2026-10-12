/**
 * The sidebar's collapsed-room state. It lives in this process only and is
 * never written to browser storage: a collapsed room is a view state of the
 * current run, not part of the session the daemon restores.
 */

import { writable, type Readable } from 'svelte/store';

import { pruneCollapsed } from './room-groups';

/** The set of collapsed room ids, with the operations the sidebar performs on it. */
export interface CollapsedRooms extends Readable<ReadonlySet<string>> {
  /**
   * Collapses a room that is expanded, expands one that is collapsed.
   *
   * @param roomId the room to flip.
   */
  toggle: (roomId: string) => void;
  /**
   * Expands a room, leaving an already expanded one expanded. Idempotent on
   * purpose: reaching a window inside a collapsed room must open that room
   * without ever closing the room a window is already visible in.
   *
   * @param roomId the room to expand.
   */
  expand: (roomId: string) => void;
  /**
   * Forgets the rooms that no longer hold a window, delegating the rule to
   * {@link pruneCollapsed}.
   *
   * @param live the room ids that currently hold a window.
   */
  prune: (live: ReadonlySet<string>) => void;
}

/** Creates a collapsed-room store with every room expanded. */
export function createCollapsedRooms(): CollapsedRooms {
  const { subscribe, update } = writable<ReadonlySet<string>>(new Set());
  return {
    subscribe,
    toggle: (roomId) => {
      update((collapsed) => {
        const next = new Set(collapsed);
        if (!next.delete(roomId)) {
          next.add(roomId);
        }
        return next;
      });
    },
    expand: (roomId) => {
      update((collapsed) => {
        const next = new Set(collapsed);
        next.delete(roomId);
        return next;
      });
    },
    prune: (live) => {
      update((collapsed) => pruneCollapsed(collapsed, live));
    },
  };
}
