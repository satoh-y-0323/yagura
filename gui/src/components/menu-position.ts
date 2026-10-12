/** An anchor rectangle, viewport-relative (the `⋯` button's bounding box). */
export interface Rect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** A width/height pair, used for both the menu and the viewport. */
export interface Size {
  width: number;
  height: number;
}

/** A viewport-relative point, ready to assign to `position: fixed` `left`/`top`. */
export interface Point {
  left: number;
  top: number;
}

/**
 * Clamps one axis into `[0, viewportSize - menuSize]`. When the menu does
 * not fit in the viewport at all (that range is empty or inverted), returns
 * 0 rather than a negative coordinate.
 */
function clampAxis(position: number, viewportSize: number, menuSize: number): number {
  return Math.min(Math.max(position, 0), Math.max(viewportSize - menuSize, 0));
}

/**
 * Computes the `position: fixed` coordinates for the `⋯` window menu.
 * Default placement is below-left of the anchor; the menu wraps to
 * the anchor's right edge and/or opens above it when that would overflow the
 * viewport, and is finally clamped into the viewport for inputs with no
 * fully-on-screen solution (an off-screen anchor, or a menu larger than the
 * viewport).
 */
export function placeMenu(anchor: Rect, menu: Size, viewport: Size): Point {
  let left = anchor.left;
  if (left + menu.width > viewport.width) {
    left = anchor.left + anchor.width - menu.width;
  }

  let top = anchor.top + anchor.height;
  if (top + menu.height > viewport.height) {
    top = anchor.top - menu.height;
  }

  return {
    left: clampAxis(left, viewport.width, menu.width),
    top: clampAxis(top, viewport.height, menu.height),
  };
}
