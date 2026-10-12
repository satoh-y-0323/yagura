/**
 * Terminal size reporting gate: keeps the tiny size a hidden or collapsed
 * container measures at from reaching the window, whose history would be
 * rewrapped to it.
 *
 * Every resize path must pass its candidate size through this gate before
 * acting on it.
 */

/** Smallest reportable column count. */
export const MIN_REPORTABLE_COLS = 10;

/** Smallest reportable row count. */
export const MIN_REPORTABLE_ROWS = 3;

/**
 * Gates whether a terminal resize candidate may be applied.
 *
 * Rejects:
 * - any size when the window is not visible
 * - sizes below the frozen floor even when visible
 * - undefined candidates (fail-safe: assume unsafe)
 *
 * @param isVisible Whether the window is currently displayed to the user.
 * @param candidate The size measured for the pane, or undefined if unavailable.
 * @returns true only if the candidate is safe to apply.
 */
export function canApplyTerminalSize(
  isVisible: boolean,
  candidate?: { cols: number; rows: number }
): boolean {
  if (!isVisible) {
    return false;
  }
  if (candidate === undefined) {
    return false;
  }
  if (candidate.cols < MIN_REPORTABLE_COLS) {
    return false;
  }
  if (candidate.rows < MIN_REPORTABLE_ROWS) {
    return false;
  }
  return true;
}
