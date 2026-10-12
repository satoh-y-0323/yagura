/** Where a pointer is in cells, and when its movement is worth sending. */

/** The cell size and the padding before the first cell, in CSS pixels. */
export interface CellMetrics {
  cellWidth: number;
  cellHeight: number;
  paddingLeft: number;
  paddingTop: number;
}

/** A cell of the view, counted from 0 at the top-left. */
export interface CellPosition {
  col: number;
  row: number;
}

/**
 * The cell under `point` (CSS pixels, relative to the element the metrics
 * are measured in). A point in the padding or past the grid is kept on the
 * nearest edge cell, as xterm.js does.
 */
export function cellAtPoint(
  point: { x: number; y: number },
  metrics: CellMetrics,
  size: { cols: number; rows: number }
): CellPosition {
  return {
    col: clampIndex(Math.floor((point.x - metrics.paddingLeft) / metrics.cellWidth), size.cols),
    row: clampIndex(Math.floor((point.y - metrics.paddingTop) / metrics.cellHeight), size.rows),
  };
}

/** A move is sent only when it reaches another cell than the last event sent. */
export function shouldSendMove(lastSent: CellPosition | null, next: CellPosition): boolean {
  return lastSent === null || lastSent.col !== next.col || lastSent.row !== next.row;
}

/** One wheel event moves one line, whatever its delta: negative is up. */
export function wheelLines(event: { deltaY: number }): -1 | 0 | 1 {
  if (event.deltaY < 0) return -1;
  if (event.deltaY > 0) return 1;
  return 0;
}

function clampIndex(value: number, count: number): number {
  return Math.min(Math.max(value, 0), Math.max(count - 1, 0));
}
