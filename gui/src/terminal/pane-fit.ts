/** How a window's pane is measured and where its input textarea sits, as plain numbers. */
import type { TerminalSize } from '../session/session-controller';
import type { CellMetrics } from './pointer-input';

/** The pane's size and its computed padding (CSS lengths in pixels, as strings). */
export interface PaneBox {
  /** A pane that is not shown has no size, whatever room its box holds. */
  isVisible: boolean;
  clientWidth: number;
  clientHeight: number;
  paddingLeft: string;
  paddingRight: string;
  paddingTop: string;
  paddingBottom: string;
}

/**
 * The whole cells that fit inside the pane's padding, or `null` when the pane
 * is not shown or has no room (not laid out yet, or a length that did not parse).
 */
export function cellsThatFit(
  box: PaneBox,
  cell: Pick<CellMetrics, 'cellWidth' | 'cellHeight'>
): TerminalSize | null {
  if (!box.isVisible) return null;
  const width = box.clientWidth - parseFloat(box.paddingLeft) - parseFloat(box.paddingRight);
  const height = box.clientHeight - parseFloat(box.paddingTop) - parseFloat(box.paddingBottom);
  if (!(width > 0) || !(height > 0)) return null;
  return { cols: Math.floor(width / cell.cellWidth), rows: Math.floor(height / cell.cellHeight) };
}

/** The textarea's style: on the cursor's cell, where the input method opens its window. */
export function inputPlacement(
  cursor: { x: number; y: number },
  metrics: CellMetrics
): { left: string; top: string; width: string; height: string } {
  return {
    left: `${metrics.paddingLeft + cursor.x}px`,
    top: `${metrics.paddingTop + cursor.y}px`,
    width: `${metrics.cellWidth}px`,
    height: `${metrics.cellHeight}px`,
  };
}
