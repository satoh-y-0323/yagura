import { describe, it, expect } from 'vitest';
import {
  DEFAULT_WINDOW_SIZE,
  MIN_WINDOW_SIZE,
  bringToFront,
  cascadeRect,
  maximizedRect,
  moveRect,
  reclampRect,
  resizeRect,
  type Rect,
  type ResizeHandle,
  type Size,
} from './float-layout';

/** Roomy area: several 720x420 windows fit side by side. */
const ROOMY_AREA: Size = { width: 1920, height: 1080 };
/** Exactly one default-sized window fits; no offset position fits. */
const ONE_WINDOW_AREA: Size = { width: 720, height: 420 };
/** Smaller than the 320x180 minimum window size. */
const SUB_MINIMUM_AREA: Size = { width: 200, height: 100 };

const ALL_HANDLES: ResizeHandle[] = [
  'top',
  'bottom',
  'left',
  'right',
  'top-left',
  'top-right',
  'bottom-left',
  'bottom-right',
];

function expectInside(rect: Rect, area: Size): void {
  expect(rect.position.x).toBeGreaterThanOrEqual(0);
  expect(rect.position.y).toBeGreaterThanOrEqual(0);
  expect(rect.position.x + rect.size.width).toBeLessThanOrEqual(area.width);
  expect(rect.position.y + rect.size.height).toBeLessThanOrEqual(area.height);
}

function expectAtLeastMinimum(rect: Rect): void {
  expect(rect.size.width).toBeGreaterThanOrEqual(MIN_WINDOW_SIZE.width);
  expect(rect.size.height).toBeGreaterThanOrEqual(MIN_WINDOW_SIZE.height);
}

describe('layout constants', () => {
  it('freezes the default window size at 720x420', () => {
    expect(DEFAULT_WINDOW_SIZE).toEqual({ width: 720, height: 420 });
  });

  it('freezes the minimum window size at 320x180', () => {
    expect(MIN_WINDOW_SIZE).toEqual({ width: 320, height: 180 });
  });
});

describe('cascadeRect in a roomy 1920x1080 area', () => {
  it('places the first window at the default size', () => {
    expect(cascadeRect(ROOMY_AREA, 0).size).toEqual({ width: 720, height: 420 });
  });

  it('does not place two consecutively opened windows at the same position', () => {
    expect(cascadeRect(ROOMY_AREA, 1).position).not.toEqual(cascadeRect(ROOMY_AREA, 0).position);
  });

  it('keeps all 20 cascaded windows inside the area and at or above the minimum size', () => {
    for (let placed = 0; placed < 20; placed += 1) {
      const rect = cascadeRect(ROOMY_AREA, placed);
      expectInside(rect, ROOMY_AREA);
      expectAtLeastMinimum(rect);
    }
  });
});

describe('cascadeRect in a 720x420 area that fits exactly one default window', () => {
  it('places the first window at the origin', () => {
    expect(cascadeRect(ONE_WINDOW_AREA, 0)).toEqual({
      position: { x: 0, y: 0 },
      size: { width: 720, height: 420 },
    });
  });

  it('clamps the second window to the origin instead of offsetting it out of the area', () => {
    expect(cascadeRect(ONE_WINDOW_AREA, 1).position).toEqual({ x: 0, y: 0 });
  });
});

describe('cascadeRect in a 200x100 area smaller than the minimum window size', () => {
  it('falls back to the minimum size at the origin', () => {
    expect(cascadeRect(SUB_MINIMUM_AREA, 0)).toEqual({
      position: { x: 0, y: 0 },
      size: { width: 320, height: 180 },
    });
  });
});

describe('moveRect', () => {
  const rect: Rect = { position: { x: 1000, y: 500 }, size: { width: 720, height: 420 } };

  it('applies the delta when the result stays inside the roomy 1920x1080 area', () => {
    expect(moveRect(ROOMY_AREA, rect, { dx: 40, dy: -30 })).toEqual({
      position: { x: 1040, y: 470 },
      size: { width: 720, height: 420 },
    });
  });

  it('clamps the window to the right and bottom edges of the area', () => {
    expect(moveRect(ROOMY_AREA, rect, { dx: 5000, dy: 5000 })).toEqual({
      position: { x: 1200, y: 660 },
      size: { width: 720, height: 420 },
    });
  });

  it('clamps the window to the origin when the delta pushes it past the top left corner', () => {
    expect(moveRect(ROOMY_AREA, rect, { dx: -5000, dy: -5000 }).position).toEqual({ x: 0, y: 0 });
  });

  it('clamps to the origin on the axis where the window is wider than the 300x200 area', () => {
    const oversized: Rect = { position: { x: 0, y: 0 }, size: { width: 320, height: 180 } };
    expect(moveRect({ width: 300, height: 200 }, oversized, { dx: 100, dy: 100 })).toEqual({
      position: { x: 0, y: 20 },
      size: { width: 320, height: 180 },
    });
  });

  it('does not mutate the rect it is given', () => {
    const original: Rect = { position: { x: 10, y: 20 }, size: { width: 720, height: 420 } };
    moveRect(ROOMY_AREA, original, { dx: 5000, dy: 5000 });
    expect(original).toEqual({ position: { x: 10, y: 20 }, size: { width: 720, height: 420 } });
  });
});

describe('resizeRect', () => {
  const rect: Rect = { position: { x: 100, y: 100 }, size: { width: 400, height: 300 } };

  it('grows the window from the bottom-right corner without moving its origin', () => {
    expect(resizeRect(ROOMY_AREA, rect, { dx: 120, dy: 80 }, 'bottom-right')).toEqual({
      position: { x: 100, y: 100 },
      size: { width: 520, height: 380 },
    });
  });

  it('stops at the minimum size and keeps the opposite corner fixed when the top-left handle is dragged past it', () => {
    expect(resizeRect(ROOMY_AREA, rect, { dx: 1000, dy: 1000 }, 'top-left')).toEqual({
      position: { x: 180, y: 220 },
      size: { width: 320, height: 180 },
    });
  });

  it('does not let the bottom-right corner grow past the edges of the area', () => {
    const atEdge: Rect = { position: { x: 1200, y: 600 }, size: { width: 720, height: 420 } };
    expect(resizeRect(ROOMY_AREA, atEdge, { dx: 500, dy: 500 }, 'bottom-right')).toEqual({
      position: { x: 1200, y: 600 },
      size: { width: 720, height: 480 },
    });
  });

  it('only changes x and width when the left edge is dragged', () => {
    expect(resizeRect(ROOMY_AREA, rect, { dx: 50, dy: 999 }, 'left')).toEqual({
      position: { x: 150, y: 100 },
      size: { width: 350, height: 300 },
    });
  });

  it('keeps the result inside the area and at or above the minimum size for every handle', () => {
    const deltas = [
      { dx: 5000, dy: 5000 },
      { dx: -5000, dy: -5000 },
    ];
    for (const handle of ALL_HANDLES) {
      for (const delta of deltas) {
        const resized = resizeRect(ROOMY_AREA, rect, delta, handle);
        expectInside(resized, ROOMY_AREA);
        expectAtLeastMinimum(resized);
      }
    }
  });
});

describe('bringToFront', () => {
  it('gives the target window the highest zOrder', () => {
    const next = bringToFront(
      new Map([
        ['a', 0],
        ['b', 1],
        ['c', 2],
      ]),
      'a'
    );
    expect(next.get('a')).toBe(Math.max(...next.values()));
  });

  it('renumbers every window so that no two windows share a zOrder', () => {
    const next = bringToFront(
      new Map([
        ['a', 0],
        ['b', 0],
        ['c', 0],
      ]),
      'b'
    );
    expect(new Set(next.values()).size).toBe(3);
  });

  it('keeps the target in front when it is already the front window', () => {
    const next = bringToFront(
      new Map([
        ['a', 0],
        ['b', 5],
      ]),
      'b'
    );
    expect(next.get('b')).toBe(Math.max(...next.values()));
  });

  it('leaves the window set unchanged for an unknown window id', () => {
    const next = bringToFront(
      new Map([
        ['a', 0],
        ['b', 1],
      ]),
      'missing-window'
    );
    expect([...next.keys()].sort()).toEqual(['a', 'b']);
  });

  it('keeps the zOrders unique for an unknown window id', () => {
    const next = bringToFront(
      new Map([
        ['a', 3],
        ['b', 3],
      ]),
      'missing-window'
    );
    expect(new Set(next.values()).size).toBe(2);
  });

  it('returns an empty result for an empty window set', () => {
    expect(bringToFront(new Map<string, number>(), 'a').size).toBe(0);
  });

  it('does not mutate the map it is given', () => {
    const zOrders = new Map([
      ['a', 0],
      ['b', 0],
    ]);
    bringToFront(zOrders, 'a');
    expect([...zOrders.entries()]).toEqual([
      ['a', 0],
      ['b', 0],
    ]);
  });
});

describe('maximizedRect', () => {
  it('fills the roomy 1920x1080 area', () => {
    expect(maximizedRect(ROOMY_AREA)).toEqual({
      position: { x: 0, y: 0 },
      size: { width: 1920, height: 1080 },
    });
  });

  it('falls back to the minimum size in a 200x100 area smaller than the minimum', () => {
    expect(maximizedRect(SUB_MINIMUM_AREA)).toEqual({
      position: { x: 0, y: 0 },
      size: { width: 320, height: 180 },
    });
  });
});

describe('reclampRect', () => {
  it('leaves a rect that already fits inside the area unchanged', () => {
    const rect: Rect = { position: { x: 100, y: 100 }, size: { width: 720, height: 420 } };
    expect(reclampRect(ROOMY_AREA, rect)).toEqual(rect);
  });

  it('gives back the pre-maximize rect when the area has not changed', () => {
    const before = cascadeRect(ROOMY_AREA, 2);
    maximizedRect(ROOMY_AREA);
    expect(reclampRect(ROOMY_AREA, before)).toEqual(before);
  });

  it('moves a rect back inside a shrunken 800x500 area without changing its size', () => {
    const rect: Rect = { position: { x: 1200, y: 600 }, size: { width: 720, height: 420 } };
    expect(reclampRect({ width: 800, height: 500 }, rect)).toEqual({
      position: { x: 80, y: 80 },
      size: { width: 720, height: 420 },
    });
  });

  it('shrinks a rect that no longer fits into a 500x300 area', () => {
    const rect: Rect = { position: { x: 100, y: 100 }, size: { width: 720, height: 420 } };
    expect(reclampRect({ width: 500, height: 300 }, rect)).toEqual({
      position: { x: 0, y: 0 },
      size: { width: 500, height: 300 },
    });
  });

  it('falls back to the minimum size at the origin when the area is smaller than the minimum', () => {
    const rect: Rect = { position: { x: 50, y: 50 }, size: { width: 720, height: 420 } };
    expect(reclampRect(SUB_MINIMUM_AREA, rect)).toEqual({
      position: { x: 0, y: 0 },
      size: { width: 320, height: 180 },
    });
  });

  it('keeps all 20 cascaded windows reachable after the area shrinks to 900x600', () => {
    const shrunk: Size = { width: 900, height: 600 };
    for (let placed = 0; placed < 20; placed += 1) {
      const reclamped = reclampRect(shrunk, cascadeRect(ROOMY_AREA, placed));
      expectInside(reclamped, shrunk);
      expectAtLeastMinimum(reclamped);
    }
  });
});
