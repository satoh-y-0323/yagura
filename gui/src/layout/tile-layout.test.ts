/**
 * `tileRects`: a row-major equal-size grid, `cols = ceil(sqrt(count))` and
 * `rows = ceil(count / cols)`, with any leftover cells left empty.
 */
import { describe, it, expect } from 'vitest';
import type { Size } from './float-layout';
import { tileRects } from './tile-layout';

/** Not evenly divisible by 2 or 3, so cell sizes must show the floor. */
const AREA: Size = { width: 1000, height: 700 };

function expectInside(rect: { position: { x: number; y: number }; size: Size }, area: Size): void {
  expect(rect.position.x).toBeGreaterThanOrEqual(0);
  expect(rect.position.y).toBeGreaterThanOrEqual(0);
  expect(rect.position.x + rect.size.width).toBeLessThanOrEqual(area.width);
  expect(rect.position.y + rect.size.height).toBeLessThanOrEqual(area.height);
}

function overlaps(
  a: { position: { x: number; y: number }; size: Size },
  b: { position: { x: number; y: number }; size: Size }
): boolean {
  const aRight = a.position.x + a.size.width;
  const aBottom = a.position.y + a.size.height;
  const bRight = b.position.x + b.size.width;
  const bBottom = b.position.y + b.size.height;
  return (
    a.position.x < bRight &&
    b.position.x < aRight &&
    a.position.y < bBottom &&
    b.position.y < aBottom
  );
}

describe('tileRects grid dimensions (cols = ceil(sqrt(count)), rows = ceil(count / cols))', () => {
  it('uses a 1x1 grid for a single window, filling the whole area', () => {
    const rects = tileRects(AREA, 1);

    expect(rects).toEqual([{ position: { x: 0, y: 0 }, size: { width: 1000, height: 700 } }]);
  });

  it('uses a 2x1 grid for two windows, side by side', () => {
    const rects = tileRects(AREA, 2);

    expect(rects).toEqual([
      { position: { x: 0, y: 0 }, size: { width: 500, height: 700 } },
      { position: { x: 500, y: 0 }, size: { width: 500, height: 700 } },
    ]);
  });

  it('uses a 2x2 grid for three windows, leaving the bottom-right cell empty', () => {
    const rects = tileRects(AREA, 3);

    expect(rects).toEqual([
      { position: { x: 0, y: 0 }, size: { width: 500, height: 350 } },
      { position: { x: 500, y: 0 }, size: { width: 500, height: 350 } },
      { position: { x: 0, y: 350 }, size: { width: 500, height: 350 } },
    ]);
  });

  it('uses a 2x2 grid for four windows, filling every cell', () => {
    const rects = tileRects(AREA, 4);

    expect(rects).toEqual([
      { position: { x: 0, y: 0 }, size: { width: 500, height: 350 } },
      { position: { x: 500, y: 0 }, size: { width: 500, height: 350 } },
      { position: { x: 0, y: 350 }, size: { width: 500, height: 350 } },
      { position: { x: 500, y: 350 }, size: { width: 500, height: 350 } },
    ]);
  });

  it('uses a 3x2 grid for five windows, leaving one cell empty', () => {
    const rects = tileRects(AREA, 5);

    expect(rects).toEqual([
      { position: { x: 0, y: 0 }, size: { width: 333, height: 350 } },
      { position: { x: 333, y: 0 }, size: { width: 333, height: 350 } },
      { position: { x: 666, y: 0 }, size: { width: 333, height: 350 } },
      { position: { x: 0, y: 350 }, size: { width: 333, height: 350 } },
      { position: { x: 333, y: 350 }, size: { width: 333, height: 350 } },
    ]);
  });
});

describe('tileRects cell size: every cell is the same, floored size', () => {
  it('sizes every cell to floor(area.width / cols) x floor(area.height / rows)', () => {
    const rects = tileRects(AREA, 5);
    const expectedSize: Size = { width: 333, height: 350 };

    for (const rect of rects) {
      expect(rect.size).toEqual(expectedSize);
    }
  });
});

describe('tileRects layout invariants: containment and non-overlap', () => {
  it('keeps every cell inside the area', () => {
    const rects = tileRects(AREA, 5);

    for (const rect of rects) {
      expectInside(rect, AREA);
    }
  });

  it('never overlaps two cells', () => {
    const rects = tileRects(AREA, 5);

    for (let i = 0; i < rects.length; i += 1) {
      for (let j = i + 1; j < rects.length; j += 1) {
        const a = rects[i];
        const b = rects[j];
        if (a === undefined || b === undefined) {
          continue;
        }
        expect(overlaps(a, b)).toBe(false);
      }
    }
  });
});

describe('tileRects degenerate inputs return an empty array', () => {
  it('returns an empty array when count is zero', () => {
    expect(tileRects(AREA, 0)).toEqual([]);
  });

  it('returns an empty array when count is negative', () => {
    expect(tileRects(AREA, -1)).toEqual([]);
  });

  it('returns an empty array when the area width is zero', () => {
    expect(tileRects({ width: 0, height: 700 }, 3)).toEqual([]);
  });

  it('returns an empty array when the area height is zero', () => {
    expect(tileRects({ width: 1000, height: 0 }, 3)).toEqual([]);
  });

  it('returns an empty array when the area width is negative', () => {
    expect(tileRects({ width: -10, height: 700 }, 3)).toEqual([]);
  });

  it('returns an empty array when the area height is negative', () => {
    expect(tileRects({ width: 1000, height: -10 }, 3)).toEqual([]);
  });
});

/**
 * A large `count` over a positive area floors `cellWidth` / `cellHeight` below 1px
 * even though every degenerate-input guard above passes; that is degenerate too.
 */
describe('tileRects degenerate cell size: floored cell dimensions below 1px', () => {
  it('returns an empty array when count makes the floored cell width less than 1px', () => {
    // cols = ceil(sqrt(100)) = 10; cellWidth = floor(5 / 10) = 0.
    expect(tileRects({ width: 5, height: 700 }, 100)).toEqual([]);
  });

  it('returns an empty array when count makes the floored cell height less than 1px', () => {
    // cols = ceil(sqrt(50)) = 8, rows = ceil(50 / 8) = 7; cellHeight = floor(5 / 7) = 0.
    expect(tileRects({ width: 1000, height: 5 }, 50)).toEqual([]);
  });
});

describe('tileRects with nothing to tile', () => {
  it.each([
    ['a zero count', AREA, 0],
    ['a negative count', AREA, -3],
    ['a zero width', { width: 0, height: 700 }, 2],
    ['a negative width', { width: -10, height: 700 }, 2],
    ['a zero height', { width: 1000, height: 0 }, 2],
    ['a negative height', { width: 1000, height: -10 }, 2],
  ])('returns no rectangles for %s', (_label, area, count) => {
    expect(tileRects(area, count)).toEqual([]);
  });

  it('returns no rectangles when a cell would be narrower than 1px', () => {
    expect(tileRects({ width: 1, height: 700 }, 2)).toEqual([]);
  });

  it('returns no rectangles when a cell would be shorter than 1px', () => {
    expect(tileRects({ width: 1000, height: 1 }, 3)).toEqual([]);
  });

  it('still tiles when every cell is exactly 1px wide and tall', () => {
    expect(tileRects({ width: 2, height: 2 }, 4)).toEqual([
      { position: { x: 0, y: 0 }, size: { width: 1, height: 1 } },
      { position: { x: 1, y: 0 }, size: { width: 1, height: 1 } },
      { position: { x: 0, y: 1 }, size: { width: 1, height: 1 } },
      { position: { x: 1, y: 1 }, size: { width: 1, height: 1 } },
    ]);
  });
});
