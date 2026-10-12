/**
 * Tile grid layout for arranging windows into an equal-size grid.
 * Pure functions with no DOM or Svelte dependencies.
 */

import type { Rect, Size } from './float-layout';

/**
 * Computes the row-major grid rectangles that tile `count` windows into
 * `area`. `cols = ceil(sqrt(count))` and `rows = ceil(count / cols)`; every
 * cell is `floor(area.width / cols) x floor(area.height / rows)`, so a count
 * that does not fill the grid leaves the trailing cells empty (e.g. 3
 * windows use a 2x2 grid with only the bottom-right cell empty).
 * @param area Available area dimensions
 * @param count Number of windows to place
 * @returns Rectangles in row-major fill order; empty when `count` or either
 *   `area` dimension is not positive, or when a floored cell dimension comes
 *   out below 1px
 */
export function tileRects(area: Size, count: number): Rect[] {
  const cols = Math.ceil(Math.sqrt(count));
  const rows = Math.ceil(count / cols);
  const cellWidth = Math.floor(area.width / cols);
  const cellHeight = Math.floor(area.height / rows);
  // A non-positive count or dimension makes a cell size NaN, zero or negative; all fail this test.
  if (!(cellWidth >= 1 && cellHeight >= 1)) {
    return [];
  }

  const rects: Rect[] = [];
  for (let i = 0; i < count; i += 1) {
    const col = i % cols;
    const row = Math.floor(i / cols);
    rects.push({
      position: { x: col * cellWidth, y: row * cellHeight },
      size: { width: cellWidth, height: cellHeight },
    });
  }
  return rects;
}
