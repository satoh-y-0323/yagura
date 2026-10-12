import { describe, it, expect } from 'vitest';
import { cellAtPoint, shouldSendMove, wheelLines, type CellMetrics } from './pointer-input';

// Cells of 8 x 16 CSS pixels inside a 4 / 2 pixel padding, on an 80 x 24 grid.
const METRICS: CellMetrics = { cellWidth: 8, cellHeight: 16, paddingLeft: 4, paddingTop: 2 };
const SIZE = { cols: 80, rows: 24 };

describe('the cell under a pointer', () => {
  it('counts cells from the padding edge', () => {
    expect(cellAtPoint({ x: 4, y: 2 }, METRICS, SIZE)).toEqual({ col: 0, row: 0 });
    expect(cellAtPoint({ x: 4 + 8 * 3 + 0.5, y: 2 + 16 * 5 + 0.5 }, METRICS, SIZE)).toEqual({
      col: 3,
      row: 5,
    });
  });

  it('keeps a point just before a cell edge in the earlier cell', () => {
    expect(cellAtPoint({ x: 4 + 8 * 3 - 0.01, y: 2 + 16 * 5 - 0.01 }, METRICS, SIZE)).toEqual({
      col: 2,
      row: 4,
    });
  });

  it('keeps a point in the padding or past the grid on the nearest edge cell', () => {
    expect(cellAtPoint({ x: 0, y: 0 }, METRICS, SIZE)).toEqual({ col: 0, row: 0 });
    expect(cellAtPoint({ x: -50, y: -50 }, METRICS, SIZE)).toEqual({ col: 0, row: 0 });
    expect(cellAtPoint({ x: 4 + 8 * 80 + 30, y: 2 + 16 * 24 + 30 }, METRICS, SIZE)).toEqual({
      col: 79,
      row: 23,
    });
  });
});

describe('sending a move only when the pointer reaches another cell', () => {
  it('sends the first move', () => {
    expect(shouldSendMove(null, { col: 3, row: 4 })).toBe(true);
  });

  it('does not send a move to the cell of the last pointer event sent', () => {
    expect(shouldSendMove({ col: 3, row: 4 }, { col: 3, row: 4 })).toBe(false);
  });

  it('sends a move to another column or another row', () => {
    expect(shouldSendMove({ col: 3, row: 4 }, { col: 4, row: 4 })).toBe(true);
    expect(shouldSendMove({ col: 3, row: 4 }, { col: 3, row: 5 })).toBe(true);
  });
});

describe('one wheel event moves one line', () => {
  it('moves up one line for any upward delta', () => {
    expect(wheelLines({ deltaY: -1 })).toBe(-1);
    expect(wheelLines({ deltaY: -120 })).toBe(-1);
    expect(wheelLines({ deltaY: -3000 })).toBe(-1);
  });

  it('moves down one line for any downward delta', () => {
    expect(wheelLines({ deltaY: 0.5 })).toBe(1);
    expect(wheelLines({ deltaY: 3 })).toBe(1);
    expect(wheelLines({ deltaY: 3000 })).toBe(1);
  });

  it('moves nothing for an event without vertical movement', () => {
    expect(wheelLines({ deltaY: 0 })).toBe(0);
  });
});
