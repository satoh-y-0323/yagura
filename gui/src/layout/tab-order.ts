/**
 * Tab order arithmetic for the tab strip.
 *
 * The strip owns no ordering logic: it hands over ids and pointer coordinates
 * and reads back an order or a drop slot. Keeping that arithmetic
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
