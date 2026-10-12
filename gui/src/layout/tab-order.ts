/**
 * Tab order arithmetic for the tab strip.
 *
 * The strip owns no ordering logic: it hands over ids and pointer coordinates
 * and reads back an order, a drop slot or a successor. Keeping that arithmetic
 * here, free of DOM and store access, is what makes it testable in the `node`
 * test environment, where no component can be mounted.
 */

/** Pointer travel, in pixels, that separates a tab click from a tab drag. */
export const TAB_DRAG_THRESHOLD_PX = 5;

/** Horizontal extent of one rendered tab. */
export interface TabBounds {
  /** Id of the pane this tab stands for. */
  windowId: string;
  /** Left edge of the tab, in the same coordinate space as the pointer. */
  left: number;
  /** Rendered width of the tab. */
  width: number;
}

/** A drag in progress: the tabs on screen, the held tab, and the pointer travel. */
export interface TabDropQuery {
  /** Every tab currently rendered, in the order it is rendered. */
  tabs: readonly TabBounds[];
  /** Id of the held tab. */
  windowId: string;
  /** Pointer position where the drag started. */
  originX: number;
  /** Current pointer position. */
  pointerX: number;
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
 * Returns the position the held tab would land on, or `null` while the gesture
 * is still a click: the pointer has not left the drag threshold, or the held id
 * has no tab (which also covers a strip with no tabs at all).
 *
 * The slot is the number of *other* tab centres the pointer has passed, so a
 * tab dropped where it was picked up keeps its position.
 */
export function resolveDropIndex(query: TabDropQuery): number | null {
  const { tabs, windowId, originX, pointerX } = query;
  if (!tabs.some((tab) => tab.windowId === windowId)) {
    return null;
  }
  if (Math.abs(pointerX - originX) <= TAB_DRAG_THRESHOLD_PX) {
    return null;
  }
  const others = tabs.filter((tab) => tab.windowId !== windowId);
  return others.filter((tab) => tab.left + tab.width / 2 < pointerX).length;
}

/**
 * Returns the tab to activate once `removedId` leaves the strip: the nearest
 * eligible tab after it, else the nearest eligible tab before it, else `null`.
 *
 * `null` means there is nothing left to show as a tab, which the caller reads
 * as "leave tab mode". Eligibility is supplied by the caller so this
 * stays independent of what makes a pane showable.
 *
 * `order` is the order *before* the removal: `removedId` is what locates the
 * neighbours, so a caller that has already dropped it can only be answered
 * from the front of the strip.
 */
export function nextActiveTab(
  order: readonly string[],
  isEligible: (windowId: string) => boolean,
  removedId: string
): string | null {
  const from = order.indexOf(removedId);
  const start = from === -1 ? 0 : from;
  const candidates = order.filter((id) => id !== removedId);
  const after = candidates.slice(start).find((id) => isEligible(id));
  if (after !== undefined) {
    return after;
  }
  const before = candidates
    .slice(0, start)
    .reverse()
    .find((id) => isEligible(id));
  return before ?? null;
}
