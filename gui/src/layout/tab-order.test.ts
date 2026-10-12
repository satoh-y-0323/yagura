/**
 * Tab order arithmetic. The gui test environment is `node`, so the tab strip keeps
 * its ordering logic in pure functions here.
 */
import { describe, it, expect } from 'vitest';

import { nextActiveTab, reorderTabs, resolveDropIndex, type TabBounds } from './tab-order';

/** Three 100px-wide tabs laid out edge to edge; centres are 50 / 150 / 250. */
const THREE_TABS: TabBounds[] = [
  { windowId: 'w-a', left: 0, width: 100 },
  { windowId: 'w-b', left: 100, width: 100 },
  { windowId: 'w-c', left: 200, width: 100 },
];

const ALL_ELIGIBLE = (): boolean => true;

/** Eligibility stands in for "not minimized" in production. */
function eligibleOnly(...windowIds: string[]): (windowId: string) => boolean {
  const allowed = new Set(windowIds);
  return (windowId) => allowed.has(windowId);
}

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

describe('nextActiveTab: picks the successor from the tab order', () => {
  it('picks the tab that follows the removed one', () => {
    expect(nextActiveTab(['w-a', 'w-b', 'w-c'], ALL_ELIGIBLE, 'w-a')).toBe('w-b');
  });

  it('picks the preceding tab when the removed one was last', () => {
    expect(nextActiveTab(['w-a', 'w-b', 'w-c'], ALL_ELIGIBLE, 'w-c')).toBe('w-b');
  });

  it('follows the tab order rather than the order the windows were opened in', () => {
    // The order a drag left behind: w-c was moved to the front.
    expect(nextActiveTab(['w-c', 'w-a', 'w-b'], ALL_ELIGIBLE, 'w-c')).toBe('w-a');
  });

  it('skips a tab that is not eligible and takes the next eligible one', () => {
    expect(nextActiveTab(['w-a', 'w-b', 'w-c'], eligibleOnly('w-c'), 'w-a')).toBe('w-c');
  });

  it('falls back to an eligible tab before the removed one', () => {
    expect(nextActiveTab(['w-a', 'w-b', 'w-c'], eligibleOnly('w-a'), 'w-b')).toBe('w-a');
  });

  it('returns nothing when the removed tab was the only eligible one', () => {
    expect(nextActiveTab(['w-a', 'w-b', 'w-c'], eligibleOnly('w-a'), 'w-a')).toBeNull();
  });

  it('returns nothing when no tab is eligible', () => {
    expect(nextActiveTab(['w-a', 'w-b'], eligibleOnly(), 'w-a')).toBeNull();
  });

  it('returns nothing when the last tab is removed', () => {
    expect(nextActiveTab(['w-a'], ALL_ELIGIBLE, 'w-a')).toBeNull();
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

  it('prefers the nearest eligible tab before the removed one over a farther one', () => {
    expect(nextActiveTab(['w-a', 'w-b', 'w-c', 'w-d'], eligibleOnly('w-a', 'w-b'), 'w-d')).toBe(
      'w-b'
    );
  });

  it('answers from the front of the strip when the removed id is not in the order', () => {
    expect(nextActiveTab(['w-a', 'w-b', 'w-c'], ALL_ELIGIBLE, 'w-x')).toBe('w-a');
    expect(nextActiveTab(['w-a', 'w-b', 'w-c'], eligibleOnly('w-b', 'w-c'), 'w-x')).toBe('w-b');
  });
});
