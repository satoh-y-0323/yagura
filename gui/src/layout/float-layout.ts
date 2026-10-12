/**
 * Floating window layout utilities for multi-window management.
 * Pure functions with no DOM or Svelte dependencies.
 */

/**
 * Size with width and height dimensions.
 */
export type Size = {
  width: number;
  height: number;
};

/**
 * Rectangle with position and size.
 */
export type Rect = {
  position: {
    x: number;
    y: number;
  };
  size: Size;
};

/**
 * Resize handle identifier for window corners and edges.
 */
export type ResizeHandle =
  'top' | 'bottom' | 'left' | 'right' | 'top-left' | 'top-right' | 'bottom-left' | 'bottom-right';

/** Default window size. */
export const DEFAULT_WINDOW_SIZE: Size = {
  width: 720,
  height: 420,
};

/** Minimum window size. */
export const MIN_WINDOW_SIZE: Size = {
  width: 320,
  height: 180,
};

const CASCADE_OFFSET = 40;

/**
 * Calculate a cascaded rectangle position for a new window.
 * Uses a deterministic cascade pattern with wrapping.
 * @param area Available area dimensions
 * @param assignedCount Number of windows already placed
 * @returns Rectangle positioned in cascade pattern
 */
export function cascadeRect(area: Size, assignedCount: number): Rect {
  const size: Size =
    area.width >= MIN_WINDOW_SIZE.width && area.height >= MIN_WINDOW_SIZE.height
      ? {
          width: Math.min(DEFAULT_WINDOW_SIZE.width, area.width),
          height: Math.min(DEFAULT_WINDOW_SIZE.height, area.height),
        }
      : MIN_WINDOW_SIZE;

  const maxColumns = Math.max(1, Math.floor((area.width - size.width) / CASCADE_OFFSET) + 1);
  const column = assignedCount % maxColumns;
  const row = Math.floor(assignedCount / maxColumns);

  // The column count keeps x inside the area; only the row can run past the bottom.
  const x = column * CASCADE_OFFSET;
  const y = Math.max(0, Math.min(row * CASCADE_OFFSET, area.height - size.height));

  return {
    position: { x, y },
    size,
  };
}

/**
 * Move a rectangle by applying a delta offset.
 * Clamps the result to stay within the area bounds.
 * @param area Available area
 * @param rect Rectangle to move
 * @param delta Offset to apply
 * @returns New rectangle (input is not mutated)
 */
export function moveRect(area: Size, rect: Rect, delta: { dx: number; dy: number }): Rect {
  let newX = rect.position.x + delta.dx;
  let newY = rect.position.y + delta.dy;

  newX = Math.max(0, Math.min(newX, area.width - rect.size.width));
  newY = Math.max(0, Math.min(newY, area.height - rect.size.height));

  return {
    position: { x: newX, y: newY },
    size: rect.size,
  };
}

/**
 * Resize a rectangle from a given handle while keeping the opposite corner fixed.
 * Respects minimum size and area bounds.
 * @param area Available area
 * @param rect Rectangle to resize
 * @param delta Drag offset
 * @param handle Which handle is being dragged
 * @returns New rectangle (input is not mutated)
 */
export function resizeRect(
  area: Size,
  rect: Rect,
  delta: { dx: number; dy: number },
  handle: ResizeHandle
): Rect {
  const opposite = {
    x: rect.position.x + rect.size.width,
    y: rect.position.y + rect.size.height,
  };

  let minX = rect.position.x;
  let maxX = opposite.x;
  let minY = rect.position.y;
  let maxY = opposite.y;

  if (handle === 'left' || handle === 'top-left' || handle === 'bottom-left') {
    minX += delta.dx;
  }
  if (handle === 'right' || handle === 'top-right' || handle === 'bottom-right') {
    maxX += delta.dx;
  }
  if (handle === 'top' || handle === 'top-left' || handle === 'top-right') {
    minY += delta.dy;
  }
  if (handle === 'bottom' || handle === 'bottom-left' || handle === 'bottom-right') {
    maxY += delta.dy;
  }

  if (maxX - minX < MIN_WINDOW_SIZE.width) {
    // Dragging a right edge below the minimum is caught by the final minimum width.
    if (handle === 'left' || handle === 'top-left' || handle === 'bottom-left') {
      minX = maxX - MIN_WINDOW_SIZE.width;
    }
  }
  if (maxY - minY < MIN_WINDOW_SIZE.height) {
    if (handle === 'top' || handle === 'top-left' || handle === 'top-right') {
      minY = maxY - MIN_WINDOW_SIZE.height;
    }
  }

  minX = Math.max(0, minX);
  minY = Math.max(0, minY);
  maxX = Math.min(area.width, maxX);
  maxY = Math.min(area.height, maxY);

  const finalWidth = Math.max(MIN_WINDOW_SIZE.width, maxX - minX);
  const finalHeight = Math.max(MIN_WINDOW_SIZE.height, maxY - minY);

  const finalX = Math.min(minX, area.width - finalWidth);
  const finalY = Math.min(minY, area.height - finalHeight);

  return {
    position: { x: Math.max(0, finalX), y: Math.max(0, finalY) },
    size: { width: finalWidth, height: finalHeight },
  };
}

/**
 * Bring a window to the front by assigning it the highest z-order.
 * Renumbers all windows to ensure unique z-orders.
 * @param zOrders Map of window IDs to z-order values
 * @param windowId Window to bring to front
 * @returns New map with updated z-orders (input is not mutated)
 */
export function bringToFront(zOrders: Map<string, number>, windowId: string): Map<string, number> {
  const ids = Array.from(zOrders.entries())
    .sort((a, b) => a[1] - b[1])
    .map(([id]) => id);
  const front = ids.includes(windowId) ? [...ids.filter((id) => id !== windowId), windowId] : ids;
  return new Map(front.map((id, order) => [id, order]));
}

/**
 * Get the maximized rectangle that fills the entire area.
 * Falls back to minimum size at origin if area is too small.
 * @param area Available area
 * @returns Rectangle filling the area
 */
export function maximizedRect(area: Size): Rect {
  const size: Size =
    area.width >= MIN_WINDOW_SIZE.width && area.height >= MIN_WINDOW_SIZE.height
      ? area
      : MIN_WINDOW_SIZE;

  return {
    position: { x: 0, y: 0 },
    size,
  };
}

/**
 * Clamp a rectangle to fit within an area, adjusting position and size as needed.
 * Preserves size if possible, then shrinks if necessary.
 * @param area Available area
 * @param rect Rectangle to clamp
 * @returns Clamped rectangle (input is not mutated)
 */
export function reclampRect(area: Size, rect: Rect): Rect {
  if (area.width < MIN_WINDOW_SIZE.width || area.height < MIN_WINDOW_SIZE.height) {
    return {
      position: { x: 0, y: 0 },
      size: MIN_WINDOW_SIZE,
    };
  }

  let newWidth = rect.size.width;
  let newHeight = rect.size.height;
  let newX = rect.position.x;
  let newY = rect.position.y;

  if (newWidth > area.width) {
    newWidth = area.width;
    newX = 0;
  }
  if (newHeight > area.height) {
    newHeight = area.height;
    newY = 0;
  }

  newX = Math.max(0, Math.min(newX, area.width - newWidth));
  newY = Math.max(0, Math.min(newY, area.height - newHeight));

  return {
    position: { x: newX, y: newY },
    size: { width: newWidth, height: newHeight },
  };
}
