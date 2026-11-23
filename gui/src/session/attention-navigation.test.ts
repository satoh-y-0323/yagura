/** The sidebar's "go to the next window that needs you" jump. */
import { describe, it, expect } from 'vitest';
import { get } from 'svelte/store';

import { formatPathForDisplay, type DisplayLine } from '../display/untrusted-text';
import type { DetectedStatusView, WindowStatus } from '../ipc/session-boundary';
import { nextNeedsYouView } from '../sidebar/needs-you-view';
import { groupByRoom } from '../sidebar/room-groups';
import { createCollapsedRooms, type CollapsedRooms } from '../sidebar/collapsed-rooms';
import {
  createAttentionNavigation,
  nextWindowNeedingYou,
  type AttentionNavigation,
} from './attention-navigation';

// ---------------------------------------------------------------------------
// Fixture: two rooms whose screen order differs from the global tab order
// ---------------------------------------------------------------------------

interface Entry {
  windowId: string;
  cwd: string;
  displayCwd: DisplayLine;
  room: { roomId: string; displayName: DisplayLine } | null;
  detected: DetectedStatusView;
  display: { isMinimized: boolean; zOrder: number };
}

/** The windows in the order they were opened, which fixes the room order. */
const LAYOUT: ReadonlyArray<readonly [string, string]> = [
  ['a1', 'room-a'],
  ['a2', 'room-a'],
  ['b1', 'room-b'],
  ['b2', 'room-b'],
];

/**
 * A tab order that is neither the order the windows were opened in nor the
 * screen order: the rooms keep their own order (room-a, then room-b) while
 * each room follows `tabOrder`, so the list reads a2, a1, b2, b1 on screen
 * while the global tab order reads a2, b2, a1, b1. A cycle derived from
 * `tabOrder` alone instead of from the list on screen therefore
 * visits the waiting windows in the opposite order.
 */
const TAB_ORDER = ['a2', 'b2', 'a1', 'b1'];

/** The screen order the fixture produces, top to bottom. */
const SCREEN_ORDER = ['a2', 'a1', 'b2', 'b1'];

const FRONT_Z_ORDER = 50;

function makeEntry(windowId: string, roomId: string, status: WindowStatus, zOrder: number): Entry {
  return {
    windowId,
    cwd: `C:\\Users\\dev\\${roomId}`,
    displayCwd: formatPathForDisplay(`C:\\Users\\dev\\${roomId}`),
    room: { roomId, displayName: formatPathForDisplay(roomId) },
    detected: status === 'unknown' ? { status, reason: null } : { status },
    display: { isMinimized: false, zOrder },
  };
}

/**
 * The four windows of the two-room fixture. `waiting` names the ones whose
 * status makes them need the user; `front` names the one holding the highest
 * `zOrder` on the canvas, which is not where the jump starts: it starts from
 * the current window it is given.
 */
function fourWindows(waiting: readonly string[], front = 'b1'): Entry[] {
  return LAYOUT.map(([windowId, roomId]) =>
    makeEntry(
      windowId,
      roomId,
      waiting.includes(windowId) ? 'waiting_input' : 'running',
      windowId === front ? FRONT_Z_ORDER : 1
    )
  );
}

function groupsOf(entries: readonly Entry[], collapsed: ReadonlySet<string> = new Set()) {
  return groupByRoom(entries, collapsed, TAB_ORDER);
}

/** The room order and the order within each room, as the sidebar renders them. */
function orderSnapshot(entries: readonly Entry[], collapsed: ReadonlySet<string>) {
  return groupsOf(entries, collapsed).map((group) => ({
    roomId: group.roomId,
    entries: group.entries.map((entry) => entry.windowId),
  }));
}

interface Harness {
  navigation: AttentionNavigation<Entry>;
  collapsedRooms: CollapsedRooms;
  /** Every `expand:<roomId>` / `goTo:<windowId>` call, in the order they were made. */
  calls: string[];
  focused: string[];
  currentEntries: () => readonly Entry[];
  /** Moves a window out of the waiting set, as typing into it would. */
  stopWaiting: (windowId: string) => void;
}

/**
 * Wires the jump to a real {@link CollapsedRooms} store, so "expands a
 * collapsed room" and "does not collapse one that is already open" are read
 * from the store's own state rather than from a stub that cannot be wrong.
 */
function harnessOf(
  entries: readonly Entry[],
  collapsedRoomIds: readonly string[],
  current: string | null
): Harness {
  const collapsedRooms = createCollapsedRooms();
  for (const roomId of collapsedRoomIds) {
    collapsedRooms.toggle(roomId);
  }
  const state = { entries: [...entries] };
  const calls: string[] = [];
  const focused: string[] = [];
  let currentId = current;

  const navigation = createAttentionNavigation<Entry>({
    groups: () => groupsOf(state.entries, get(collapsedRooms)),
    current: () => (currentId === null ? null : { windowId: currentId }),
    view: () => null,
    expandRoom: (roomId) => {
      calls.push(`expand:${roomId}`);
      collapsedRooms.expand(roomId);
    },
    focusTerminal: (windowId) => {
      calls.push(`terminal:${windowId}`);
    },
    goTo: (entry) => {
      calls.push(`goTo:${entry.windowId}`);
      focused.push(entry.windowId);
      // Models going to a window in Focus: it becomes the current window and
      // the placement is left alone.
      currentId = entry.windowId;
    },
  });

  return {
    navigation,
    collapsedRooms,
    calls,
    focused,
    currentEntries: () => state.entries,
    stopWaiting: (windowId) => {
      state.entries = state.entries.map((candidate) =>
        candidate.windowId === windowId
          ? { ...candidate, detected: { status: 'running' } }
          : candidate
      );
    },
  };
}

// ---------------------------------------------------------------------------
// Which window the jump picks
// ---------------------------------------------------------------------------

describe('nextWindowNeedingYou: the next waiting window in screen order', () => {
  it('reads the list top to bottom, not in tab order: from the current window a2 it picks a1, the window the screen shows next', () => {
    // In tab order the waiting windows read b2, a1 -- the opposite order --
    // so picking b2 here would mean the cycle followed `tabOrder` globally.
    const target = nextWindowNeedingYou(groupsOf(fourWindows(['a1', 'b2'])), { windowId: 'a2' });

    expect(target?.entry.windowId).toBe('a1');
  });

  it('starts from the current window it is given, not from the window placed in front', () => {
    // Focus does not move the placement: a2 stays in front while a1 is shown.
    const target = nextWindowNeedingYou(groupsOf(fourWindows(['a1', 'b2'], 'a2')), {
      windowId: 'a1',
    });

    expect(target?.entry.windowId).toBe('b2');

    // With no current window the search starts from the top of the list, whatever the placement:
    // a2 is in front and waiting; starting from it would pick b2.
    const fromTop = nextWindowNeedingYou(groupsOf(fourWindows(['a2', 'b2'], 'a2')), null);
    expect(fromTop?.entry.windowId).toBe('a2');
  });

  it('crosses into the next room when the current window is the previous target', () => {
    const target = nextWindowNeedingYou(groupsOf(fourWindows(['a1', 'b2'])), { windowId: 'a1' });

    expect(target?.entry.windowId).toBe('b2');
  });

  it('names the room of the window it returns, so the caller can expand it', () => {
    const groups = groupsOf(fourWindows(['b2']));

    // groups[1] is room-b, whose first entry on screen is b2.
    expect(groups[1]?.entries[0]?.windowId).toBe('b2');

    const target = nextWindowNeedingYou(groups, { windowId: 'a2' });

    expect(target).toEqual({ entry: groups[1]?.entries[0], roomId: 'room-b' });
  });

  it('wraps to the first waiting window when the current window sits below the last one', () => {
    const target = nextWindowNeedingYou(groupsOf(fourWindows(['a1', 'b2'])), { windowId: 'b1' });

    expect(target?.entry.windowId).toBe('a1');
  });

  it('skips the current window itself when it is waiting, and wraps past the end', () => {
    const target = nextWindowNeedingYou(groupsOf(fourWindows(['a1', 'b2'])), { windowId: 'b2' });

    expect(target?.entry.windowId).toBe('a1');
  });

  it('returns the only waiting window even when it is the current window (jumping back to it is harmless)', () => {
    const target = nextWindowNeedingYou(groupsOf(fourWindows(['a1'])), { windowId: 'a1' });

    expect(target?.entry.windowId).toBe('a1');
  });

  it('returns null when no window is waiting', () => {
    expect(nextWindowNeedingYou(groupsOf(fourWindows([])), { windowId: 'a2' })).toBeNull();
  });

  it('returns null when there is no room at all', () => {
    expect(nextWindowNeedingYou(groupsOf([]), null)).toBeNull();
  });

  it('reaches a waiting window inside a collapsed room (collapsing hides a room, it does not mute it)', () => {
    const target = nextWindowNeedingYou(groupsOf(fourWindows(['b2']), new Set(['room-b'])), {
      windowId: 'a2',
    });

    expect(target?.entry.windowId).toBe('b2');
  });
});

// ---------------------------------------------------------------------------
// What the jump does, and what it must leave alone
// ---------------------------------------------------------------------------

describe('createAttentionNavigation: expanding and focusing', () => {
  it('expands the target room before focusing the window, landing where the peek said it would', () => {
    // a2 and b2 wait with the current window a1 between them: from the top the
    // answer would be a2, from a1 it is b2.
    const harness = harnessOf(fourWindows(['a2', 'b2']), ['room-a', 'room-b'], 'a1');
    expect(nextWindowNeedingYou(groupsOf(harness.currentEntries()), null)?.entry.windowId).toBe(
      'a2'
    );

    const peeked = harness.navigation.peekNextWindowNeedingYou();

    expect(peeked?.windowId).toBe('b2');
    expect(harness.calls).toEqual([]);
    expect(get(harness.collapsedRooms)).toEqual(new Set(['room-a', 'room-b']));

    const landed = harness.navigation.goToNextWindowNeedingYou();

    expect(landed).toBe(peeked);
    expect(landed).toBe(harness.currentEntries()[3]);
    expect(harness.calls).toEqual(['expand:room-b', 'goTo:b2', 'terminal:b2']);
    expect(get(harness.collapsedRooms).has('room-b')).toBe(false);
  });

  it('leaves a room that is already expanded expanded (an expand written as a toggle would close it)', () => {
    const harness = harnessOf(fourWindows(['b2']), [], 'a2');

    harness.navigation.goToNextWindowNeedingYou();

    expect(get(harness.collapsedRooms)).toEqual(new Set());
    expect(harness.focused).toEqual(['b2']);
  });

  it('leaves the collapsed state of every other room alone (nothing the user did not ask for moves)', () => {
    const harness = harnessOf(fourWindows(['a1']), ['room-a', 'room-b'], 'a2');

    harness.navigation.goToNextWindowNeedingYou();

    expect(get(harness.collapsedRooms)).toEqual(new Set(['room-b']));
  });

  it('does not collapse the room it jumped into afterwards', () => {
    const harness = harnessOf(fourWindows(['a1', 'b2']), ['room-a'], 'a2');

    harness.navigation.goToNextWindowNeedingYou();
    harness.navigation.goToNextWindowNeedingYou();

    expect(get(harness.collapsedRooms)).toEqual(new Set());
  });

  it('changes neither the room order nor the order within a room', () => {
    const harness = harnessOf(fourWindows(['a1', 'b2']), [], 'a2');
    const before = orderSnapshot(harness.currentEntries(), get(harness.collapsedRooms));

    harness.navigation.goToNextWindowNeedingYou();

    const after = orderSnapshot(harness.currentEntries(), get(harness.collapsedRooms));
    expect(after).toEqual(before);
    expect(after.flatMap((group) => group.entries)).toEqual(SCREEN_ORDER);
  });

  it('does nothing at all when no window is waiting', () => {
    const harness = harnessOf(fourWindows([]), ['room-b'], 'a2');

    expect(harness.navigation.peekNextWindowNeedingYou()).toBeNull();
    const landed = harness.navigation.goToNextWindowNeedingYou();

    expect(landed).toBeNull();
    expect(harness.calls).toEqual([]);
    expect(get(harness.collapsedRooms)).toEqual(new Set(['room-b']));
  });
});

describe('createAttentionNavigation: repeated presses (no cursor is held)', () => {
  it('does not skip the next waiting window when the one it just jumped to stops waiting', () => {
    // Waiting in screen order: a1, b2, b1. A cursor remembering "I handed out
    // index 0" would read index 1 of the shrunken list (b2, b1) and hand out
    // b1, skipping b2.
    const harness = harnessOf(fourWindows(['a1', 'b2', 'b1']), [], 'a2');

    harness.navigation.goToNextWindowNeedingYou();
    harness.stopWaiting('a1');
    harness.navigation.goToNextWindowNeedingYou();

    expect(harness.focused).toEqual(['a1', 'b2']);
  });

  it('walks the waiting windows in screen order and wraps back to the first', () => {
    const harness = harnessOf(fourWindows(['a1', 'b2']), [], 'a2');

    harness.navigation.goToNextWindowNeedingYou();
    harness.navigation.goToNextWindowNeedingYou();
    harness.navigation.goToNextWindowNeedingYou();

    expect(harness.focused).toEqual(['a1', 'b2', 'a1']);
  });
});

describe('nextWindowNeedingYou: while the Needs you filter is on', () => {
  function filteredView() {
    const first = nextNeedsYouView(null, groupsOf(fourWindows(['a1', 'b1'])), true);
    // a2 and b2 started waiting and a1 stopped: rows a1 (settled), b1, a2, b2, whereas the sidebar reads a2, a1, b2, b1.
    const groups = groupsOf(fourWindows(['a2', 'b1', 'b2']));
    return { groups, view: nextNeedsYouView(first, groups, false) };
  }

  it('walks the rows in the view order, not the sidebar order', () => {
    const { groups, view } = filteredView();
    expect(view.rows.map((row) => row.windowId)).toEqual(['a1', 'b1', 'a2', 'b2']);

    const target = nextWindowNeedingYou(groups, { windowId: 'a1' }, view);

    expect(target?.entry.windowId).toBe('b1');
  });

  it('skips a settled row and starts from the top of the rows when there is no current window', () => {
    const { groups, view } = filteredView();

    const target = nextWindowNeedingYou(groups, null, view);

    expect(target?.entry.windowId).toBe('b1');
  });

  it('walks the sidebar order when the filter is off', () => {
    const target = nextWindowNeedingYou(
      groupsOf(fourWindows(['a1', 'b2'])),
      { windowId: 'a2' },
      null
    );

    expect(target?.entry.windowId).toBe('a1');
  });
});
