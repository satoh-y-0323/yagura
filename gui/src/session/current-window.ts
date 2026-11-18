/**
 * The window keyboard operations act on, derived from the work view, and the
 * next / previous window in the sidebar's order. No second record of "the
 * current window" exists anywhere: everything reads this derivation, and the
 * only way in takes a {@link WorkView}, so no caller can skip the mode.
 */

/** The part of a window entry the derivation reads. */
export interface PlacedWindow {
  windowId: string;
  display: { zOrder: number; isMinimized: boolean; isMaximized: boolean };
}

/**
 * What the current window is derived from: the canvas placement, or the one
 * window Focus shows (null when Focus shows none).
 */
export type WorkView = { mode: 'canvas' } | { mode: 'focus'; focusedWindowId: string | null };

/** The work area's mode: one window filling it, or the floating canvas. */
export type WorkMode = WorkView['mode'];

/**
 * Whether a window is on the canvas. Kept private so that no caller can read
 * the placement while Focus decides what is shown.
 *
 * @param maximizedEntry the maximized window, else undefined; every other
 * window is hidden behind it.
 */
function isOnCanvas(entry: PlacedWindow, maximizedEntry: PlacedWindow | undefined): boolean {
  return (
    !entry.display.isMinimized &&
    (maximizedEntry === undefined || entry.windowId === maximizedEntry.windowId)
  );
}

/**
 * The canvas's current window: the maximized one if it is on screen, otherwise
 * the on-screen window with the highest `zOrder` (the later one in the list
 * wins a tie).
 */
function canvasCurrent<T extends PlacedWindow>(windows: readonly T[]): T | null {
  const maximized = windows.find((entry) => entry.display.isMaximized);
  let picked: T | null = null;
  for (const entry of windows) {
    if (!isOnCanvas(entry, maximized)) {
      continue;
    }
    if (picked === null || entry.display.zOrder >= picked.display.zOrder) {
      picked = entry;
    }
  }
  return picked;
}

/**
 * The window keyboard operations act on. Canvas reads the placement; Focus
 * reads only the window it shows and ignores the canvas's minimize mark,
 * because Focus has no way to un-minimize.
 *
 * @returns null when no window is on screen, or when the window Focus shows is
 * not in `windows`.
 */
export function currentWindowFor<T extends PlacedWindow>(
  view: WorkView,
  windows: readonly T[]
): T | null {
  if (view.mode === 'canvas') {
    return canvasCurrent(windows);
  }
  const { focusedWindowId } = view;
  if (focusedWindowId === null) {
    return null;
  }
  return windows.find((entry) => entry.windowId === focusedWindowId) ?? null;
}

/**
 * Whether a window is on screen: the single definition shared by the view
 * (what is drawn and sized) and {@link currentWindowFor} (what is operated on).
 * In Focus only the current window is on screen.
 */
export function isWindowVisibleFor<T extends PlacedWindow>(
  view: WorkView,
  entry: T,
  windows: readonly T[]
): boolean {
  if (view.mode === 'canvas') {
    return isOnCanvas(
      entry,
      windows.find((other) => other.display.isMaximized)
    );
  }
  return currentWindowFor(view, windows)?.windowId === entry.windowId;
}

/**
 * The window `step` places from `current` in `order`, wrapping at both ends.
 * A `current` that is null or absent from `order` steps to the first window
 * going forward and the last going backward.
 *
 * @returns null when `order` is empty.
 */
export function neighborWindow<T extends { windowId: string }>(
  order: readonly T[],
  current: { windowId: string } | null,
  step: 1 | -1
): T | null {
  const index = current === null ? -1 : order.findIndex((e) => e.windowId === current.windowId);
  if (index === -1) {
    return (step === 1 ? order[0] : order[order.length - 1]) ?? null;
  }
  return order[(index + step + order.length) % order.length] ?? null;
}
