import { describe, it, expect } from 'vitest';
import {
  bringToFront,
  cascadeRect,
  maximizedRect,
  reclampRect,
  resizeRect,
  type Rect,
  type ResizeHandle,
  type Size,
} from './float-layout';

const ROOMY_AREA: Size = { width: 1920, height: 1080 };
const MIN_RECT: Rect = { position: { x: 0, y: 0 }, size: { width: 320, height: 180 } };

describe('cascadeRect offsets', () => {
  it('steps 40px to the right per placed window until the row is full, then 40px down', () => {
    expect(cascadeRect(ROOMY_AREA, 1).position).toEqual({ x: 40, y: 0 });
    expect(cascadeRect(ROOMY_AREA, 30).position).toEqual({ x: 1200, y: 0 });
    expect(cascadeRect(ROOMY_AREA, 31).position).toEqual({ x: 0, y: 40 });
    expect(cascadeRect(ROOMY_AREA, 33).position).toEqual({ x: 80, y: 40 });
  });

  it('wraps to the next row when the area is only slightly wider than the window', () => {
    const area: Size = { width: 760, height: 500 };
    expect(cascadeRect(area, 1).position).toEqual({ x: 40, y: 0 });
    expect(cascadeRect(area, 2).position).toEqual({ x: 0, y: 40 });
    expect(cascadeRect(area, 3).position).toEqual({ x: 40, y: 40 });
  });

  it('clamps the row of a late window to the bottom edge of the area', () => {
    expect(cascadeRect(ROOMY_AREA, 31 * 40).position).toEqual({ x: 0, y: 660 });
  });

  it('shrinks the window to an area that is smaller than the default but not below the minimum', () => {
    expect(cascadeRect({ width: 400, height: 200 }, 0)).toEqual({
      position: { x: 0, y: 0 },
      size: { width: 400, height: 200 },
    });
  });

  it('falls back to the minimum size when only one dimension is below the minimum', () => {
    expect(cascadeRect({ width: 200, height: 500 }, 0).size).toEqual(MIN_RECT.size);
    expect(cascadeRect({ width: 500, height: 100 }, 0).size).toEqual(MIN_RECT.size);
  });
});

describe('resizeRect per handle', () => {
  const rect: Rect = { position: { x: 100, y: 100 }, size: { width: 400, height: 300 } };
  const small = { dx: 30, dy: 20 };

  const grown: Array<[ResizeHandle, Rect]> = [
    ['top', { position: { x: 100, y: 120 }, size: { width: 400, height: 280 } }],
    ['bottom', { position: { x: 100, y: 100 }, size: { width: 400, height: 320 } }],
    ['left', { position: { x: 130, y: 100 }, size: { width: 370, height: 300 } }],
    ['right', { position: { x: 100, y: 100 }, size: { width: 430, height: 300 } }],
    ['top-left', { position: { x: 130, y: 120 }, size: { width: 370, height: 280 } }],
    ['top-right', { position: { x: 100, y: 120 }, size: { width: 430, height: 280 } }],
    ['bottom-left', { position: { x: 130, y: 100 }, size: { width: 370, height: 320 } }],
    ['bottom-right', { position: { x: 100, y: 100 }, size: { width: 430, height: 320 } }],
  ];

  it.each(grown)('moves only the edges of the %s handle', (handle, expected) => {
    expect(resizeRect(ROOMY_AREA, rect, small, handle)).toEqual(expected);
  });

  const squeezed: Array<[ResizeHandle, { dx: number; dy: number }, Rect]> = [
    [
      'left',
      { dx: 1000, dy: 0 },
      { position: { x: 180, y: 100 }, size: { width: 320, height: 300 } },
    ],
    [
      'right',
      { dx: -1000, dy: 0 },
      { position: { x: 100, y: 100 }, size: { width: 320, height: 300 } },
    ],
    [
      'top',
      { dx: 0, dy: 1000 },
      { position: { x: 100, y: 220 }, size: { width: 400, height: 180 } },
    ],
    [
      'bottom',
      { dx: 0, dy: -1000 },
      { position: { x: 100, y: 100 }, size: { width: 400, height: 180 } },
    ],
    [
      'top-right',
      { dx: -1000, dy: 1000 },
      { position: { x: 100, y: 220 }, size: { width: 320, height: 180 } },
    ],
    [
      'bottom-left',
      { dx: 1000, dy: -1000 },
      { position: { x: 180, y: 100 }, size: { width: 320, height: 180 } },
    ],
    [
      'bottom-right',
      { dx: -1000, dy: -1000 },
      { position: { x: 100, y: 100 }, size: { width: 320, height: 180 } },
    ],
  ];

  it.each(squeezed)('stops at the minimum size for the %s handle', (handle, delta, expected) => {
    expect(resizeRect(ROOMY_AREA, rect, delta, handle)).toEqual(expected);
  });

  it('stops the left and top edges at the origin of the area', () => {
    expect(resizeRect(ROOMY_AREA, rect, { dx: -500, dy: -500 }, 'top-left')).toEqual({
      position: { x: 0, y: 0 },
      size: { width: 500, height: 400 },
    });
  });

  it('stops the right and bottom edges at the far edge of the area', () => {
    const area: Size = { width: 600, height: 500 };
    expect(resizeRect(area, rect, { dx: 500, dy: 500 }, 'bottom-right')).toEqual({
      position: { x: 100, y: 100 },
      size: { width: 500, height: 400 },
    });
  });

  it('pulls a rect that sticks out of the area back inside', () => {
    const area: Size = { width: 400, height: 300 };
    const sticking: Rect = { position: { x: 300, y: 200 }, size: { width: 400, height: 300 } };
    expect(resizeRect(area, sticking, { dx: 0, dy: 0 }, 'right')).toEqual({
      position: { x: 80, y: 120 },
      size: { width: 320, height: 180 },
    });
  });
});

describe('bringToFront ordering', () => {
  it('moves the target last and closes the gap it leaves, keeping the others in order', () => {
    const next = bringToFront(
      new Map([
        ['a', 5],
        ['b', 2],
        ['c', 9],
      ]),
      'a'
    );
    expect(Object.fromEntries(next)).toEqual({ b: 0, a: 2, c: 1 });
  });

  it('keeps the other windows in their order when the target is in front already', () => {
    const next = bringToFront(
      new Map([
        ['a', 10],
        ['b', 2],
        ['c', 4],
      ]),
      'a'
    );
    expect(Object.fromEntries(next)).toEqual({ b: 0, c: 1, a: 2 });
  });

  it('moves a back window to the front', () => {
    const next = bringToFront(
      new Map([
        ['a', 1],
        ['b', 2],
        ['c', 3],
        ['d', 4],
      ]),
      'a'
    );
    expect(Object.fromEntries(next)).toEqual({ b: 0, c: 1, d: 2, a: 3 });
  });

  it('only compacts the order when the id is unknown', () => {
    const next = bringToFront(
      new Map([
        ['a', 7],
        ['b', 3],
      ]),
      'missing'
    );
    expect(Object.fromEntries(next)).toEqual({ b: 0, a: 1 });
  });
});

describe('maximizedRect and reclampRect with one dimension below the minimum', () => {
  it('maximizedRect falls back to the minimum size when only the width is too small', () => {
    expect(maximizedRect({ width: 200, height: 500 }).size).toEqual(MIN_RECT.size);
  });

  it('maximizedRect falls back to the minimum size when only the height is too small', () => {
    expect(maximizedRect({ width: 500, height: 100 }).size).toEqual(MIN_RECT.size);
  });

  it('reclampRect falls back to the minimum rect when only the width is too small', () => {
    const rect: Rect = { position: { x: 5, y: 5 }, size: { width: 400, height: 300 } };
    expect(reclampRect({ width: 200, height: 500 }, rect)).toEqual(MIN_RECT);
  });

  it('reclampRect falls back to the minimum rect when only the height is too small', () => {
    const rect: Rect = { position: { x: 5, y: 5 }, size: { width: 400, height: 300 } };
    expect(reclampRect({ width: 500, height: 100 }, rect)).toEqual(MIN_RECT);
  });

  it('reclampRect shrinks one dimension without touching the other', () => {
    const tall: Rect = { position: { x: 50, y: 60 }, size: { width: 400, height: 900 } };
    expect(reclampRect({ width: 800, height: 600 }, tall)).toEqual({
      position: { x: 50, y: 0 },
      size: { width: 400, height: 600 },
    });
    const wide: Rect = { position: { x: 50, y: 60 }, size: { width: 900, height: 300 } };
    expect(reclampRect({ width: 800, height: 600 }, wide)).toEqual({
      position: { x: 0, y: 60 },
      size: { width: 800, height: 300 },
    });
  });
});

describe('an area exactly as wide or as tall as the minimum window is big enough', () => {
  it('cascadeRect sizes the window to the area rather than to the minimum', () => {
    expect(cascadeRect({ width: 320, height: 300 }, 0).size).toEqual({ width: 320, height: 300 });
    expect(cascadeRect({ width: 500, height: 180 }, 0).size).toEqual({ width: 500, height: 180 });
  });

  it('maximizedRect fills the area', () => {
    expect(maximizedRect({ width: 320, height: 300 }).size).toEqual({ width: 320, height: 300 });
    expect(maximizedRect({ width: 500, height: 180 }).size).toEqual({ width: 500, height: 180 });
  });

  it('reclampRect keeps the rect where it fits', () => {
    const narrow: Rect = { position: { x: 0, y: 40 }, size: { width: 320, height: 100 } };
    expect(reclampRect({ width: 320, height: 300 }, narrow)).toEqual(narrow);
    const short: Rect = { position: { x: 40, y: 0 }, size: { width: 200, height: 180 } };
    expect(reclampRect({ width: 500, height: 180 }, short)).toEqual(short);
  });
});

describe('bringToFront with three windows', () => {
  it('only compacts the order when the id is unknown', () => {
    const next = bringToFront(
      new Map([
        ['a', 7],
        ['b', 3],
        ['c', 5],
      ]),
      'missing'
    );
    expect(Object.fromEntries(next)).toEqual({ b: 0, c: 1, a: 2 });
  });

  it('moves a middle window to the front', () => {
    const next = bringToFront(
      new Map([
        ['a', 1],
        ['b', 2],
        ['c', 3],
      ]),
      'b'
    );
    expect(Object.fromEntries(next)).toEqual({ a: 0, c: 1, b: 2 });
  });
});
