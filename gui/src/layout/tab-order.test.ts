/**
 * Tab order arithmetic. The gui test environment is `node`, so the tab strip keeps
 * its ordering logic in pure functions here.
 */
import { describe, it, expect } from 'vitest';

import { reorderTabs, resolveDropIndex, type TabBounds } from './tab-order';

/** Three 100px-wide tabs laid out edge to edge; centres are 50 / 150 / 250. */
const THREE_TABS: TabBounds[] = [
  { windowId: 'w-a', left: 0, width: 100 },
  { windowId: 'w-b', left: 100, width: 100 },
  { windowId: 'w-c', left: 200, width: 100 },
];

describe('reorderTabs: moves one tab within the order', () => {
  it('moves a tab to the front', () => {
    expect(reorderTabs(['w-a', 'w-b', 'w-c'], 'w-c', 0)).toEqual(['w-c', 'w-a', 'w-b']);
  });

  it('moves a tab to the end', () => {
    expect(reorderTabs(['w-a', 'w-b', 'w-c'], 'w-a', 2)).toEqual(['w-b', 'w-c', 'w-a']);
  });

  it('leaves the order unchanged when the target index is the tab position it already has', () => {
    expect(reorderTabs(['w-a', 'w-b', 'w-c'], 'w-b', 1)).toEqual(['w-a', 'w-b', 'w-c']);
  });

  it('leaves the order unchanged for a window id that is not in it', () => {
    expect(reorderTabs(['w-a', 'w-b'], 'w-missing', 0)).toEqual(['w-a', 'w-b']);
  });

  it('returns an empty order for an empty order', () => {
    expect(reorderTabs([], 'w-a', 0)).toEqual([]);
  });

  it('clamps a target index past the end to the last position', () => {
    expect(reorderTabs(['w-a', 'w-b', 'w-c'], 'w-a', 99)).toEqual(['w-b', 'w-c', 'w-a']);
  });

  it('clamps a negative target index to the first position', () => {
    expect(reorderTabs(['w-a', 'w-b', 'w-c'], 'w-c', -4)).toEqual(['w-c', 'w-a', 'w-b']);
  });

  it('keeps the same ids, without duplicates and without losses', () => {
    const before = ['w-a', 'w-b', 'w-c', 'w-d'];

    const after = reorderTabs(before, 'w-b', 3);

    expect([...after].sort()).toEqual([...before].sort());
    expect(new Set(after).size).toBe(after.length);
  });

  it('does not mutate the order it was given', () => {
    const before = ['w-a', 'w-b', 'w-c'];

    reorderTabs(before, 'w-a', 2);

    expect(before).toEqual(['w-a', 'w-b', 'w-c']);
  });
});

describe('resolveDropIndex: the drag threshold separates a click from a drag', () => {
  it('returns no drop slot while the pointer is still inside the threshold', () => {
    expect(
      resolveDropIndex({ tabs: THREE_TABS, windowId: 'w-a', originX: 10, pointerX: 15 })
    ).toBeNull();
  });

  it('returns a drop slot once the pointer moves outside the threshold', () => {
    expect(
      resolveDropIndex({ tabs: THREE_TABS, windowId: 'w-a', originX: 10, pointerX: 16 })
    ).not.toBeNull();
  });

  it('applies the threshold to leftward movement as well', () => {
    expect(
      resolveDropIndex({ tabs: THREE_TABS, windowId: 'w-b', originX: 150, pointerX: 144 })
    ).not.toBeNull();
  });
});

describe('resolveDropIndex: maps a pointer position to a drop slot', () => {
  it('drops before every other tab when the pointer is left of all their centres', () => {
    expect(resolveDropIndex({ tabs: THREE_TABS, windowId: 'w-c', originX: 250, pointerX: 4 })).toBe(
      0
    );
  });

  it('drops after every other tab when the pointer is right of all their centres', () => {
    expect(
      resolveDropIndex({ tabs: THREE_TABS, windowId: 'w-a', originX: 50, pointerX: 290 })
    ).toBe(2);
  });

  it('moves one slot when the pointer has passed exactly one neighbour centre', () => {
    expect(
      resolveDropIndex({ tabs: THREE_TABS, windowId: 'w-a', originX: 50, pointerX: 160 })
    ).toBe(1);
  });

  it('returns no drop slot for a window id that has no tab', () => {
    expect(
      resolveDropIndex({ tabs: THREE_TABS, windowId: 'w-missing', originX: 10, pointerX: 290 })
    ).toBeNull();
  });

  it('returns no drop slot when there are no tabs', () => {
    expect(resolveDropIndex({ tabs: [], windowId: 'w-a', originX: 10, pointerX: 290 })).toBeNull();
  });
});

describe('tab order edge cases', () => {
  it('has not passed a tab centre while the pointer is exactly on it', () => {
    expect(
      resolveDropIndex({ tabs: THREE_TABS, windowId: 'w-a', originX: 10, pointerX: 150 })
    ).toBe(0);
    expect(
      resolveDropIndex({ tabs: THREE_TABS, windowId: 'w-a', originX: 10, pointerX: 151 })
    ).toBe(1);
  });
});
