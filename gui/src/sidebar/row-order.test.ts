/**
 * Ordering arithmetic for the Navigator's window rows: where a dragged row
 * lands, how the Move up / Move down menu items reorder a room, and where the
 * row keys go. The gui test environment is `node`, so the judgement lives in
 * pure functions.
 */
import { describe, it, expect } from 'vitest';

import { formatPathForDisplay } from '../display/untrusted-text';
import { groupByRoom } from './room-groups';

/** The module under test is loaded per test so a missing function fails only its own test. */
async function rowOrder() {
  return import('./row-order');
}

/** Three 100px-tall rows stacked edge to edge; centres are 50 / 150 / 250. */
const THREE_ROWS = [
  { windowId: 'w-a', top: 0, height: 100 },
  { windowId: 'w-b', top: 100, height: 100 },
  { windowId: 'w-c', top: 200, height: 100 },
];

describe('reorderTabs: moves one row within the order', () => {
  it('moves a row to the front', async () => {
    const { reorderTabs } = await rowOrder();
    expect(reorderTabs(['w-a', 'w-b', 'w-c'], 'w-c', 0)).toEqual(['w-c', 'w-a', 'w-b']);
  });

  it('moves a row to the end', async () => {
    const { reorderTabs } = await rowOrder();
    expect(reorderTabs(['w-a', 'w-b', 'w-c'], 'w-a', 2)).toEqual(['w-b', 'w-c', 'w-a']);
  });

  it('leaves the order unchanged when the target index is the position it already has', async () => {
    const { reorderTabs } = await rowOrder();
    expect(reorderTabs(['w-a', 'w-b', 'w-c'], 'w-b', 1)).toEqual(['w-a', 'w-b', 'w-c']);
  });

  it('leaves the order unchanged for a window id that is not in it', async () => {
    const { reorderTabs } = await rowOrder();
    expect(reorderTabs(['w-a', 'w-b'], 'w-missing', 0)).toEqual(['w-a', 'w-b']);
  });

  it('returns an empty order for an empty order', async () => {
    const { reorderTabs } = await rowOrder();
    expect(reorderTabs([], 'w-a', 0)).toEqual([]);
  });

  it('clamps a target index past the end to the last position', async () => {
    const { reorderTabs } = await rowOrder();
    expect(reorderTabs(['w-a', 'w-b', 'w-c'], 'w-a', 99)).toEqual(['w-b', 'w-c', 'w-a']);
  });

  it('clamps a negative target index to the first position', async () => {
    const { reorderTabs } = await rowOrder();
    expect(reorderTabs(['w-a', 'w-b', 'w-c'], 'w-c', -4)).toEqual(['w-c', 'w-a', 'w-b']);
  });

  it('keeps the same ids, without duplicates and without losses', async () => {
    const { reorderTabs } = await rowOrder();
    const before = ['w-a', 'w-b', 'w-c', 'w-d'];

    const after = reorderTabs(before, 'w-b', 3);

    expect([...after].sort()).toEqual([...before].sort());
    expect(new Set(after).size).toBe(after.length);
  });

  it('does not mutate the order it was given', async () => {
    const { reorderTabs } = await rowOrder();
    const before = ['w-a', 'w-b', 'w-c'];

    reorderTabs(before, 'w-a', 2);

    expect(before).toEqual(['w-a', 'w-b', 'w-c']);
  });
});

describe('resolveDropIndex: the drag threshold separates a click from a drag', () => {
  it('returns no drop slot while the pointer is still inside the threshold', async () => {
    const { resolveDropIndex } = await rowOrder();
    expect(
      resolveDropIndex({ rows: THREE_ROWS, windowId: 'w-a', originY: 10, pointerY: 15 })
    ).toBeNull();
  });

  it('returns a drop slot once the pointer moves outside the threshold', async () => {
    const { resolveDropIndex } = await rowOrder();
    expect(
      resolveDropIndex({ rows: THREE_ROWS, windowId: 'w-a', originY: 10, pointerY: 16 })
    ).not.toBeNull();
  });

  it('applies the threshold to upward movement as well', async () => {
    const { resolveDropIndex } = await rowOrder();
    expect(
      resolveDropIndex({ rows: THREE_ROWS, windowId: 'w-b', originY: 150, pointerY: 144 })
    ).not.toBeNull();
  });
});

describe('resolveDropIndex: maps a pointer position to a drop slot', () => {
  it('drops before every other row when the pointer is above all their centres', async () => {
    const { resolveDropIndex } = await rowOrder();
    expect(resolveDropIndex({ rows: THREE_ROWS, windowId: 'w-c', originY: 250, pointerY: 4 })).toBe(
      0
    );
  });

  it('drops after every other row when the pointer is below all their centres', async () => {
    const { resolveDropIndex } = await rowOrder();
    expect(
      resolveDropIndex({ rows: THREE_ROWS, windowId: 'w-a', originY: 50, pointerY: 290 })
    ).toBe(2);
  });

  it('moves one slot when the pointer has passed exactly one neighbour centre', async () => {
    const { resolveDropIndex } = await rowOrder();
    expect(
      resolveDropIndex({ rows: THREE_ROWS, windowId: 'w-a', originY: 50, pointerY: 160 })
    ).toBe(1);
  });

  it('returns no drop slot for a window id that has no row', async () => {
    const { resolveDropIndex } = await rowOrder();
    expect(
      resolveDropIndex({ rows: THREE_ROWS, windowId: 'w-missing', originY: 10, pointerY: 290 })
    ).toBeNull();
  });

  it('returns no drop slot when there are no rows', async () => {
    const { resolveDropIndex } = await rowOrder();
    expect(resolveDropIndex({ rows: [], windowId: 'w-a', originY: 10, pointerY: 290 })).toBeNull();
  });

  it('has not passed a row centre while the pointer is exactly on it', async () => {
    const { resolveDropIndex } = await rowOrder();
    expect(
      resolveDropIndex({ rows: THREE_ROWS, windowId: 'w-a', originY: 10, pointerY: 150 })
    ).toBe(0);
    expect(
      resolveDropIndex({ rows: THREE_ROWS, windowId: 'w-a', originY: 10, pointerY: 151 })
    ).toBe(1);
  });
});

describe('moveWithinRoom: Move up / Move down reorder a room one step', () => {
  const roomA = { roomId: 'room-a', displayName: formatPathForDisplay('Room A') };
  const roomB = { roomId: 'room-b', displayName: formatPathForDisplay('Room B') };
  const displayCwd = formatPathForDisplay('c');
  const entries = [
    { windowId: 'a1', cwd: 'c', displayCwd, room: roomA, display: { isMinimized: false } },
    { windowId: 'b1', cwd: 'c', displayCwd, room: roomB, display: { isMinimized: false } },
    { windowId: 'a2', cwd: 'c', displayCwd, room: roomA, display: { isMinimized: false } },
    { windowId: 'b2', cwd: 'c', displayCwd, room: roomB, display: { isMinimized: false } },
    { windowId: 'a3', cwd: 'c', displayCwd, room: roomA, display: { isMinimized: false } },
  ];
  // Room A reads a1, a2, a3 and room B reads b1, b2 once grouped; the two rooms interleave here.
  const tabOrder = ['a1', 'b1', 'a2', 'b2', 'a3'];

  function roomsAfter(order: readonly string[]): string[][] {
    return groupByRoom(entries, new Set(), order).map((group) =>
      group.entries.map((entry) => entry.windowId)
    );
  }

  it('moves the row one place up inside its own room and leaves the other room alone', async () => {
    const { moveWithinRoom } = await rowOrder();
    const groups = groupByRoom(entries, new Set(), tabOrder);

    const next = moveWithinRoom(tabOrder, groups, 'a2', 'up');

    expect(roomsAfter(next)).toEqual([
      ['a2', 'a1', 'a3'],
      ['b1', 'b2'],
    ]);
  });

  it('moves the row one place down inside its own room', async () => {
    const { moveWithinRoom } = await rowOrder();
    const groups = groupByRoom(entries, new Set(), tabOrder);

    const next = moveWithinRoom(tabOrder, groups, 'b1', 'down');

    expect(roomsAfter(next)).toEqual([
      ['a1', 'a2', 'a3'],
      ['b2', 'b1'],
    ]);
  });

  it('does nothing at the top of a room, even when another room sits above in the order', async () => {
    const { moveWithinRoom } = await rowOrder();
    const groups = groupByRoom(entries, new Set(), tabOrder);

    const next = moveWithinRoom(tabOrder, groups, 'b1', 'up');

    expect(roomsAfter(next)).toEqual([
      ['a1', 'a2', 'a3'],
      ['b1', 'b2'],
    ]);
  });

  it('does nothing at the bottom of a room', async () => {
    const { moveWithinRoom } = await rowOrder();
    const groups = groupByRoom(entries, new Set(), tabOrder);

    const next = moveWithinRoom(tabOrder, groups, 'a3', 'down');

    expect(roomsAfter(next)).toEqual([
      ['a1', 'a2', 'a3'],
      ['b1', 'b2'],
    ]);
  });

  it('keeps every id exactly once and does not mutate the order it was given', async () => {
    const { moveWithinRoom } = await rowOrder();
    const groups = groupByRoom(entries, new Set(), tabOrder);
    const before = [...tabOrder];

    const next = moveWithinRoom(tabOrder, groups, 'a2', 'down');

    expect([...next].sort()).toEqual([...before].sort());
    expect(tabOrder).toEqual(before);
  });
});

describe('rowKeyTarget: where the row keys go in the list', () => {
  const ids = ['w-a', 'w-b', 'w-c'];

  it('moves one row down and one row up', async () => {
    const { rowKeyTarget } = await rowOrder();
    expect(rowKeyTarget(ids, 'w-b', 'ArrowDown')).toBe('w-c');
    expect(rowKeyTarget(ids, 'w-b', 'ArrowUp')).toBe('w-a');
  });

  it('stops at the last row instead of wrapping to the first', async () => {
    const { rowKeyTarget } = await rowOrder();
    expect(rowKeyTarget(ids, 'w-c', 'ArrowDown')).toBe('w-c');
  });

  it('stops at the first row instead of wrapping to the last', async () => {
    const { rowKeyTarget } = await rowOrder();
    expect(rowKeyTarget(ids, 'w-a', 'ArrowUp')).toBe('w-a');
  });

  it('goes to the first row on Home and the last row on End', async () => {
    const { rowKeyTarget } = await rowOrder();
    expect(rowKeyTarget(ids, 'w-b', 'Home')).toBe('w-a');
    expect(rowKeyTarget(ids, 'w-b', 'End')).toBe('w-c');
  });
});

describe('rowTabStop: the one Tab stop of the list', () => {
  const ids = ['w-a', 'w-b', 'w-c'];

  it('stops on the current window when it is among the visible rows', async () => {
    const { rowTabStop } = await rowOrder();
    expect(rowTabStop(ids, 'w-b')).toBe('w-b');
  });

  it('falls back to the first visible row when the current window is not visible', async () => {
    const { rowTabStop } = await rowOrder();
    expect(rowTabStop(ids, 'w-hidden')).toBe('w-a');
  });

  it('falls back to the first visible row when there is no current window', async () => {
    const { rowTabStop } = await rowOrder();
    expect(rowTabStop(ids, null)).toBe('w-a');
  });

  it('is null when there are no rows', async () => {
    const { rowTabStop } = await rowOrder();
    expect(rowTabStop([], 'w-a')).toBeNull();
    expect(rowTabStop([], null)).toBeNull();
  });
});
