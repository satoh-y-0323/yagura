/**
 * Bundling of window entries into the sidebar's rooms. Pure functions with no
 * DOM, IPC or Svelte dependency: the room order comes from `entries`, and the
 * order within a room follows `tabOrder`'s position (a window id absent from
 * `tabOrder` sorts to the end), so the sidebar never re-derives either.
 */

import type { DetectedStatusView } from '../ipc/session-boundary';
import { attentionCount } from '../status-tone';

/**
 * The part of a window entry that decides which room it belongs to. A window
 * without a room stands for a room of its own, keyed by its working directory.
 */
interface RoomMember {
  cwd: string;
  room: { roomId: string; name: string } | null;
}

/** One room's windows, with the counts its heading shows. */
export interface RoomGroup<T> {
  /** Identifier of the room, or the working directory when it has no room. */
  roomId: string;
  /** Display name of the room, or the working directory when it has no room. */
  name: string;
  /** The room's windows, ordered by their position in `tabOrder` (absent ids last). */
  entries: T[];
  /** Number of windows needing the user. */
  attention: number;
  /** Number of windows whose status is unknown. */
  unknown: number;
  /** Number of minimized windows. */
  minimized: number;
  /** Whether the room's windows are hidden behind its heading. */
  collapsed: boolean;
}

function roomIdOf(entry: RoomMember): string {
  return entry.room?.roomId ?? entry.cwd;
}

function roomNameOf(entry: RoomMember): string {
  return entry.room?.name ?? entry.cwd;
}

/** `windowId`'s position in `tabOrder`, or `tabOrder.length` when absent (total order). */
function tabOrderIndex(tabOrder: readonly string[], windowId: string): number {
  const index = tabOrder.indexOf(windowId);
  return index === -1 ? tabOrder.length : index;
}

/**
 * Orders `entries` by their position in `tabOrder`. Ids absent from
 * `tabOrder` sort to the end, keeping their relative input order among
 * themselves (the explicit index tie-breaker, rather than relying on
 * `Array.prototype.sort`'s stability alone).
 */
function orderByTabPosition<T extends { windowId: string }>(
  entries: readonly T[],
  tabOrder: readonly string[]
): T[] {
  return entries
    .map((entry, index) => ({ entry, index, position: tabOrderIndex(tabOrder, entry.windowId) }))
    .sort((a, b) => a.position - b.position || a.index - b.index)
    .map(({ entry }) => entry);
}

/**
 * Bundles window entries into their rooms, in the order the rooms first appear
 * in `entries`. A room with no window does not appear. The input is not
 * mutated.
 *
 * @param entries the windows to bundle.
 * @param collapsed the room ids that are currently collapsed.
 * @param tabOrder the window ids in their current tab position; decides the
 * order of entries within each room, independently of the room order.
 */
export function groupByRoom<
  T extends RoomMember & {
    windowId: string;
    detected: DetectedStatusView;
    display: { isMinimized: boolean };
  },
>(
  entries: readonly T[],
  collapsed: ReadonlySet<string>,
  tabOrder: readonly string[]
): RoomGroup<T>[] {
  const rooms = new Map<string, { name: string; entries: T[] }>();
  for (const entry of entries) {
    const roomId = roomIdOf(entry);
    const room = rooms.get(roomId);
    if (room) {
      room.entries.push(entry);
    } else {
      rooms.set(roomId, { name: roomNameOf(entry), entries: [entry] });
    }
  }
  return Array.from(rooms, ([roomId, room]) => {
    const ordered = orderByTabPosition(room.entries, tabOrder);
    return {
      roomId,
      name: room.name,
      entries: ordered,
      attention: attentionCount(ordered),
      unknown: ordered.filter((entry) => entry.detected.status === 'unknown').length,
      minimized: ordered.filter((entry) => entry.display.isMinimized).length,
      collapsed: collapsed.has(roomId),
    };
  });
}

/**
 * The windows of every room in the order the sidebar renders them, top to
 * bottom: the rooms in group order, each room in its own order. A collapsed
 * room's windows are included -- collapsing hides a room, it does not drop
 * its windows. The entries themselves are returned, not copies.
 *
 * @param groups the rooms as {@link groupByRoom} bundled them.
 */
export function windowsInDisplayOrder<T>(groups: readonly RoomGroup<T>[]): T[] {
  return groups.flatMap((group) => group.entries);
}

/**
 * Collects the ids of the windows belonging to one room, in input order.
 *
 * @param entries the windows to read.
 * @param roomId the room to collect for.
 */
export function windowIdsOfRoom<T extends RoomMember & { windowId: string }>(
  entries: readonly T[],
  roomId: string
): string[] {
  return entries.filter((entry) => roomIdOf(entry) === roomId).map((entry) => entry.windowId);
}

/**
 * Collects the ids of the rooms that currently hold a window.
 *
 * @param entries the windows to read.
 */
export function roomIdsOf(entries: readonly RoomMember[]): Set<string> {
  return new Set(entries.map(roomIdOf));
}

/**
 * Drops the room ids that no longer hold a window. The single place that
 * decides what survives; the store's `prune` delegates here.
 *
 * @param collapsed the currently collapsed room ids.
 * @param live the room ids that currently hold a window.
 * @returns a new set, leaving `collapsed` unchanged.
 */
export function pruneCollapsed(
  collapsed: ReadonlySet<string>,
  live: ReadonlySet<string>
): Set<string> {
  return new Set(Array.from(collapsed).filter((roomId) => live.has(roomId)));
}
