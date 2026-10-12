/**
 * The window keyboard operations act on, derived from what is on screen, and
 * the next / previous window in the sidebar's order. No second record of "the
 * current window" exists anywhere: everything reads this derivation.
 */

/** The part of a window entry the derivation reads. */
export interface PlacedWindow {
  windowId: string;
  display: { zOrder: number; isMinimized: boolean; isMaximized: boolean };
}

/**
 * Whether a window is on screen. This is the single definition shared by the
 * view (what is drawn) and {@link currentWindow} (what is operated on).
 *
 * @param activeTabEntry the maximized window while in tab mode, else undefined;
 * in tab mode every other window is hidden behind it.
 */
export function isWindowVisible(
  entry: PlacedWindow,
  activeTabEntry: { windowId: string } | undefined
): boolean {
  return (
    !entry.display.isMinimized &&
    (activeTabEntry === undefined || entry.windowId === activeTabEntry.windowId)
  );
}

/**
 * The window keyboard operations act on: the maximized one if it is on screen,
 * otherwise the on-screen window with the highest `zOrder` (the later one in
 * the list wins a tie).
 *
 * @returns null when no window is on screen.
 */
export function currentWindow<T extends PlacedWindow>(windows: readonly T[]): T | null {
  const activeTab = windows.find((entry) => entry.display.isMaximized);
  let picked: T | null = null;
  for (const entry of windows) {
    if (!isWindowVisible(entry, activeTab)) {
      continue;
    }
    if (picked === null || entry.display.zOrder >= picked.display.zOrder) {
      picked = entry;
    }
  }
  return picked;
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
