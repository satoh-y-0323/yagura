/**
 * Ordering arithmetic for the Navigator's window rows: where a dragged row
 * lands, how the Move up / Move down items reorder a room, and where the row
 * keys go. The rows own no ordering logic: they hand over ids and pointer
 * coordinates and read back an order, a drop slot or an id. Keeping that here,
 * free of DOM and store access, is what makes it testable in the `node` test
 * environment.
 */
import { windowsInDisplayOrder, type RoomGroup } from './room-groups';

/** Pointer travel, in pixels, that separates a row click from a row drag. */
export const ROW_DRAG_THRESHOLD_PX = 5;

/** Vertical extent of one rendered row. */
export interface RowBounds {
  /** Id of the window this row stands for. */
  windowId: string;
  /** Top edge of the row, in the same coordinate space as the pointer. */
  top: number;
  /** Rendered height of the row. */
  height: number;
}

/** A drag in progress: the rows on screen, the held row, and the pointer travel. */
export interface RowDropQuery {
  /** Every row currently rendered, in the order it is rendered. */
  rows: readonly RowBounds[];
  /** Id of the held row. */
  windowId: string;
  /** Pointer position where the drag started. */
  originY: number;
  /** Current pointer position. */
  pointerY: number;
}

/**
 * Returns `order` with `windowId` moved to `targetIndex`, clamping the index to
 * the ends of the order. An id that is not in the order (and therefore an empty
 * order) comes back unchanged. The input is never mutated.
 */
export function reorderTabs(
  order: readonly string[],
  windowId: string,
  targetIndex: number
): string[] {
  if (order.indexOf(windowId) === -1) {
    return [...order];
  }
  const rest = order.filter((id) => id !== windowId);
  const clamped = Math.min(Math.max(targetIndex, 0), rest.length);
  return [...rest.slice(0, clamped), windowId, ...rest.slice(clamped)];
}

/**
 * Returns the position the held row would land on, or `null` while the gesture
 * is still a click: the pointer has not left the drag threshold, or the held id
 * has no row (which also covers a list with no rows at all).
 *
 * The slot is the number of *other* row centres the pointer has passed, so a
 * row dropped where it was picked up keeps its position.
 */
export function resolveDropIndex(query: RowDropQuery): number | null {
  const { rows, windowId, originY, pointerY } = query;
  if (!rows.some((row) => row.windowId === windowId)) {
    return null;
  }
  if (Math.abs(pointerY - originY) <= ROW_DRAG_THRESHOLD_PX) {
    return null;
  }
  const others = rows.filter((row) => row.windowId !== windowId);
  return others.filter((row) => row.top + row.height / 2 < pointerY).length;
}

/**
 * Returns the order with `windowId` swapped with its neighbour inside the same
 * room, or the order unchanged at the room's edge or for an unknown id. Another
 * room never takes part, so a window cannot leave its room this way.
 *
 * @param order the window ids in their current order.
 * @param groups the rooms as `groupByRoom` bundled them from `order`.
 */
export function moveWithinRoom<T extends { windowId: string }>(
  order: readonly string[],
  groups: readonly RoomGroup<T>[],
  windowId: string,
  direction: 'up' | 'down'
): string[] {
  const group = groups.find((candidate) =>
    candidate.entries.some((entry) => entry.windowId === windowId)
  );
  if (group === undefined) {
    return [...order];
  }
  const index = group.entries.findIndex((entry) => entry.windowId === windowId);
  const neighbour = group.entries[direction === 'up' ? index - 1 : index + 1];
  if (neighbour === undefined) {
    return [...order];
  }
  // Windows missing from `order` sort last in a room, so appending them keeps what the room shows.
  const present = new Set(order);
  const full = [
    ...order,
    ...windowsInDisplayOrder(groups)
      .map((entry) => entry.windowId)
      .filter((id) => !present.has(id)),
  ];
  const a = full.indexOf(windowId);
  const b = full.indexOf(neighbour.windowId);
  [full[a], full[b]] = [full[b] as string, full[a] as string];
  return full;
}

/** Keys that move between rows. */
export type RowKey = 'ArrowUp' | 'ArrowDown' | 'Home' | 'End';

/**
 * The row a row key goes to. The ends stop the move instead of wrapping. An
 * unknown `currentId` is treated as sitting just outside the list, so a key
 * enters it from the nearer end.
 */
export function rowKeyTarget(ids: readonly string[], currentId: string, key: RowKey): string {
  if (ids.length === 0) {
    return currentId;
  }
  const last = ids.length - 1;
  const index = ids.indexOf(currentId);
  const target =
    key === 'Home'
      ? 0
      : key === 'End'
        ? last
        : key === 'ArrowDown'
          ? Math.min(index + 1, last)
          : index === -1
            ? last
            : Math.max(index - 1, 0);
  return ids[target] as string;
}
