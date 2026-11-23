/**
 * Bundling of window entries into rooms for the sidebar: room order, the
 * tone-based order within a room, the derived counts, and the collapsed flag.
 */
import { describe, it, expect } from 'vitest';

import { formatPathForDisplay, type DisplayLine } from '../display/untrusted-text';
import type { DetectedStatusView, WindowStatus } from '../ipc/session-boundary';
import {
  groupByRoom,
  windowIdsOfRoom,
  roomIdsOf,
  pruneCollapsed,
  windowsInDisplayOrder,
} from './room-groups';

/** The minimal shape `groupByRoom` needs, independent of the full `WindowEntry`. */
interface Entry {
  windowId: string;
  cwd: string;
  displayCwd: DisplayLine;
  detected: DetectedStatusView;
  room: { roomId: string; displayName: DisplayLine } | null;
  display: { isMinimized: boolean };
}

/** An entry whose `displayCwd` is its `cwd` prepared for display, as the registry makes it. */
function makeEntry(overrides: Partial<Entry> & { status?: WindowStatus } = {}): Entry {
  const { status = 'running', ...rest } = overrides;
  const cwd = rest.cwd ?? 'C:\\Users\\dev\\project';
  return {
    windowId: 'w-a',
    cwd,
    displayCwd: formatPathForDisplay(cwd),
    detected: status === 'unknown' ? { status, reason: null } : { status },
    room: null,
    display: { isMinimized: false },
    ...rest,
  };
}

describe('groupByRoom', () => {
  it('orders rooms by the order they first appear in entries', () => {
    const roomA = { roomId: 'room-a', displayName: formatPathForDisplay('Room A') };
    const roomB = { roomId: 'room-b', displayName: formatPathForDisplay('Room B') };
    const entries = [
      makeEntry({ windowId: 'w-a', room: roomA }),
      makeEntry({ windowId: 'w-b', room: roomB }),
      makeEntry({ windowId: 'w-c', room: roomA }),
    ];

    const groups = groupByRoom(entries, new Set(), ['w-a', 'w-b', 'w-c']);

    expect(groups.map((group) => group.roomId)).toEqual(['room-a', 'room-b']);
    expect(groups.map((group) => group.kind)).toEqual(['room', 'room']);
  });

  it('groups a null room by cwd, used as both roomId and name', () => {
    const entries = [
      makeEntry({
        windowId: 'w-a',
        room: null,
        cwd: 'C:\\Users\\dev\\solo',
      }),
    ];

    const groups = groupByRoom(entries, new Set(), ['w-a']);

    expect(groups).toHaveLength(1);
    expect(groups[0]?.roomId).toBe('C:\\Users\\dev\\solo');
    expect(groups[0]?.name).toBe('C:\\Users\\dev\\solo');
    expect(groups[0]?.kind).toBe('folder');
  });

  it('names a room and a folder by their display forms, never the raw name or cwd', () => {
    const room = { roomId: 'room-a', displayName: formatPathForDisplay('ya\u202Egura') };
    const entries = [
      makeEntry({ windowId: 'w-a', room }),
      makeEntry({
        windowId: 'w-b',
        room: null,
        cwd: 'C:\\so\u202Elo',
      }),
    ];

    const groups = groupByRoom(entries, new Set(), ['w-a', 'w-b']);

    expect(groups.map((group) => [group.roomId, group.name])).toEqual([
      ['room-a', 'ya\\u{202E}gura'],
      ['C:\\so\u202Elo', 'C:\\so\\u{202E}lo'],
    ]);
  });

  it('orders entries within a room by tabOrder position, not by tone', () => {
    const room = { roomId: 'room-a', displayName: formatPathForDisplay('Room A') };
    const entries = [
      makeEntry({ windowId: 'inactive-1', room, status: 'idle' }),
      makeEntry({ windowId: 'attention-1', room, status: 'waiting_input' }),
      makeEntry({ windowId: 'working-1', room, status: 'running' }),
      makeEntry({ windowId: 'unknown-1', room, status: 'unknown' }),
      makeEntry({ windowId: 'attention-2', room, status: 'waiting_approval' }),
      makeEntry({ windowId: 'inactive-2', room, status: 'interrupted' }),
    ];
    // Deliberately neither the tone rank order nor the `entries` array order,
    // so a match can only come from following tabOrder.
    const tabOrder = [
      'unknown-1',
      'inactive-2',
      'attention-1',
      'working-1',
      'inactive-1',
      'attention-2',
    ];

    const groups = groupByRoom(entries, new Set(), tabOrder);

    expect(groups[0]?.entries.map((entry) => entry.windowId)).toEqual(tabOrder);
  });

  it('keeps the within-room order stable when only statuses change, tabOrder unchanged', () => {
    const room = { roomId: 'room-a', displayName: formatPathForDisplay('Room A') };
    const tabOrder = ['w-a', 'w-b', 'w-c'];
    const before = [
      makeEntry({ windowId: 'w-a', room, status: 'idle' }),
      makeEntry({ windowId: 'w-b', room, status: 'running' }),
      makeEntry({ windowId: 'w-c', room, status: 'waiting_input' }),
    ];
    const after = [
      makeEntry({ windowId: 'w-a', room, status: 'waiting_approval' }),
      makeEntry({ windowId: 'w-b', room, status: 'exited' }),
      makeEntry({ windowId: 'w-c', room, status: 'idle' }),
    ];

    const idsBefore = groupByRoom(before, new Set(), tabOrder)[0]?.entries.map(
      (entry) => entry.windowId
    );
    const idsAfter = groupByRoom(after, new Set(), tabOrder)[0]?.entries.map(
      (entry) => entry.windowId
    );

    expect(idsBefore).toEqual(['w-a', 'w-b', 'w-c']);
    expect(idsAfter).toEqual(['w-a', 'w-b', 'w-c']);
  });

  it('keeps room order tied to entries, unaffected by status changes and a reordered tabOrder (rejects path B)', () => {
    const roomA = { roomId: 'room-a', displayName: formatPathForDisplay('Room A') };
    const roomB = { roomId: 'room-b', displayName: formatPathForDisplay('Room B') };
    // `entries` order is fixed: room-a is seen first (w-a1), room-b second (w-b1).
    const entries = [
      makeEntry({ windowId: 'w-a1', room: roomA, status: 'idle' }),
      makeEntry({ windowId: 'w-b1', room: roomB, status: 'running' }),
      makeEntry({ windowId: 'w-a2', room: roomA, status: 'waiting_input' }),
      makeEntry({ windowId: 'w-b2', room: roomB, status: 'exited' }),
    ];
    // Placing a room-b window first in tabOrder would flip the room order if
    // room order were (wrongly) derived by sorting entries by tabOrder first,
    // then grouping (path B), instead of grouping by `entries` order and only
    // sorting within a room by tabOrder (path A).
    const tabOrder = ['w-b2', 'w-a2', 'w-b1', 'w-a1'];

    const groups = groupByRoom(entries, new Set(), tabOrder);

    expect(groups.map((group) => group.roomId)).toEqual(['room-a', 'room-b']);
    expect(groups.map((group) => group.entries.map((entry) => entry.windowId))).toEqual([
      ['w-a2', 'w-a1'],
      ['w-b2', 'w-b1'],
    ]);

    // Change statuses and reorder tabOrder again: room order must still hold.
    const changedEntries = [
      makeEntry({ windowId: 'w-a1', room: roomA, status: 'waiting_approval' }),
      makeEntry({ windowId: 'w-b1', room: roomB, status: 'unknown' }),
      makeEntry({ windowId: 'w-a2', room: roomA, status: 'interrupted' }),
      makeEntry({ windowId: 'w-b2', room: roomB, status: 'idle' }),
    ];
    const reorderedTabOrder = ['w-b1', 'w-b2', 'w-a1', 'w-a2'];

    const groupsAfter = groupByRoom(changedEntries, new Set(), reorderedTabOrder);

    expect(groupsAfter.map((group) => group.roomId)).toEqual(['room-a', 'room-b']);
  });

  it('sets minimized to the count of entries with display.isMinimized', () => {
    const room = { roomId: 'room-a', displayName: formatPathForDisplay('Room A') };
    const entries = [
      makeEntry({ windowId: 'w-a', room, display: { isMinimized: true } }),
      makeEntry({ windowId: 'w-b', room, display: { isMinimized: false } }),
      makeEntry({ windowId: 'w-c', room, display: { isMinimized: true } }),
    ];

    const groups = groupByRoom(entries, new Set(), ['w-a', 'w-b', 'w-c']);

    expect(groups[0]?.minimized).toBe(2);
  });

  it('sets collapsed to true only for rooms present in the collapsed set', () => {
    const roomA = { roomId: 'room-a', displayName: formatPathForDisplay('Room A') };
    const roomB = { roomId: 'room-b', displayName: formatPathForDisplay('Room B') };
    const entries = [
      makeEntry({ windowId: 'w-a', room: roomA }),
      makeEntry({ windowId: 'w-b', room: roomB }),
    ];

    const groups = groupByRoom(entries, new Set(['room-b']), ['w-a', 'w-b']);

    expect(groups.find((group) => group.roomId === 'room-a')?.collapsed).toBe(false);
    expect(groups.find((group) => group.roomId === 'room-b')?.collapsed).toBe(true);
  });

  it('returns an empty array for an empty entry list', () => {
    expect(groupByRoom([], new Set(), [])).toEqual([]);
  });

  it('does not mutate its input entries', () => {
    const room = { roomId: 'room-a', displayName: formatPathForDisplay('Room A') };
    const entries = Object.freeze([
      Object.freeze(makeEntry({ windowId: 'w-a', room })),
      Object.freeze(makeEntry({ windowId: 'w-b', room, status: 'idle' })),
    ]);

    expect(() => groupByRoom(entries, new Set(), ['w-a', 'w-b'])).not.toThrow();
  });
});

describe('windowIdsOfRoom', () => {
  it('returns the window ids of a room in input order', () => {
    const room = { roomId: 'room-a', displayName: formatPathForDisplay('Room A') };
    const other = { roomId: 'room-b', displayName: formatPathForDisplay('Room B') };
    const entries = [
      makeEntry({ windowId: 'w-a', room }),
      makeEntry({ windowId: 'w-b', room: other }),
      makeEntry({ windowId: 'w-c', room }),
    ];

    expect(windowIdsOfRoom(entries, 'room-a')).toEqual(['w-a', 'w-c']);
  });

  it('returns an empty array when the room has no windows', () => {
    const entries = [
      makeEntry({
        windowId: 'w-a',
        room: { roomId: 'room-a', displayName: formatPathForDisplay('Room A') },
      }),
    ];

    expect(windowIdsOfRoom(entries, 'room-missing')).toEqual([]);
  });
});

describe('roomIdsOf', () => {
  it('collects the current room ids, using cwd for null rooms', () => {
    const entries = [
      makeEntry({
        windowId: 'w-a',
        room: { roomId: 'room-a', displayName: formatPathForDisplay('Room A') },
      }),
      makeEntry({
        windowId: 'w-b',
        room: null,
        cwd: 'C:\\Users\\dev\\solo',
      }),
    ];

    expect(roomIdsOf(entries)).toEqual(new Set(['room-a', 'C:\\Users\\dev\\solo']));
  });
});

describe('pruneCollapsed', () => {
  it('drops ids that are not in the live set', () => {
    const collapsed = new Set(['room-a', 'room-b']);
    const live = new Set(['room-a']);

    expect(pruneCollapsed(collapsed, live)).toEqual(new Set(['room-a']));
  });

  it('returns a new Set and does not mutate the input collapsed set', () => {
    const collapsed = new Set(['room-a', 'room-b']);
    const live = new Set(['room-a']);

    const pruned = pruneCollapsed(collapsed, live);

    expect(pruned).not.toBe(collapsed);
    expect(collapsed).toEqual(new Set(['room-a', 'room-b']));
  });
});

describe('windowsInDisplayOrder (the sidebar list, top to bottom)', () => {
  const roomA = { roomId: 'room-a', displayName: formatPathForDisplay('Room A') };
  const roomB = { roomId: 'room-b', displayName: formatPathForDisplay('Room B') };

  /**
   * Two rooms whose screen order is neither the order the windows were opened
   * in nor the global tab order: the rooms follow `entries` (room-a, then
   * room-b) while each room follows `tabOrder`, so the list reads a2, a1, b2,
   * b1 on screen while `tabOrder` itself reads a2, b2, a1, b1.
   */
  const entries = [
    makeEntry({ windowId: 'a1', room: roomA }),
    makeEntry({ windowId: 'a2', room: roomA }),
    makeEntry({ windowId: 'b1', room: roomB }),
    makeEntry({ windowId: 'b2', room: roomB }),
  ];
  const tabOrder = ['a2', 'b2', 'a1', 'b1'];

  it('concatenates the rooms in group order, each room in its own order', () => {
    const ordered = windowsInDisplayOrder(groupByRoom(entries, new Set(), tabOrder));

    expect(ordered.map((entry) => entry.windowId)).toEqual(['a2', 'a1', 'b2', 'b1']);
  });

  it('includes the windows of a collapsed room (collapsing hides a room, it does not drop it)', () => {
    const ordered = windowsInDisplayOrder(groupByRoom(entries, new Set(['room-b']), tabOrder));

    expect(ordered.map((entry) => entry.windowId)).toEqual(['a2', 'a1', 'b2', 'b1']);
  });

  it('returns the entries themselves, not copies', () => {
    const groups = groupByRoom(entries, new Set(), tabOrder);

    const ordered = windowsInDisplayOrder(groups);

    expect(ordered[0]).toBe(groups[0]?.entries[0]);
  });

  it('returns an empty array when there is no room', () => {
    expect(windowsInDisplayOrder([])).toEqual([]);
  });
});

describe('groupByRoom: windows missing from the tab order', () => {
  it('keeps the input order among them and puts them after the windows the tab order names', () => {
    const entries = [
      makeEntry({ windowId: 'w-1', cwd: 'same' }),
      makeEntry({ windowId: 'w-2', cwd: 'same' }),
      makeEntry({ windowId: 'w-3', cwd: 'same' }),
      makeEntry({ windowId: 'w-4', cwd: 'same' }),
    ];

    const groups = groupByRoom(entries, new Set(), ['w-4']);

    expect(groups[0]?.entries.map((entry) => entry.windowId)).toEqual(['w-4', 'w-1', 'w-2', 'w-3']);
  });
});
