/**
 * The sidebar's "go to the next window that needs you" jump.
 *
 * The module is read through `import.meta.glob` rather
 * than a direct import, so that a module this file cannot resolve reports
 * which contracts are unmet instead of failing collection of the whole file
 * and reporting 0 tests.
 */
import { describe, it, expect } from 'vitest';
import { get } from 'svelte/store';

import type { DetectedStatusView, WindowStatus } from '../ipc/session-boundary';
import { groupByRoom, type RoomGroup } from '../sidebar/room-groups';
import { createCollapsedRooms, type CollapsedRooms } from '../sidebar/collapsed-rooms';

// ---------------------------------------------------------------------------
// The contract the jump module must meet
// ---------------------------------------------------------------------------

/** The part of a window entry the jump needs. */
interface NavigableWindow {
  windowId: string;
  detected: DetectedStatusView;
  display: { zOrder: number };
}

/** The window to jump to, with the room that has to be expanded to reach it. */
interface AttentionTarget<T> {
  entry: T;
  roomId: string;
}

/** What the jump acts through; the view injects the store and the actions. */
interface AttentionNavigationDeps<T extends NavigableWindow> {
  groups: () => readonly RoomGroup<T>[];
  expandRoom: (roomId: string) => void;
  focus: (entry: T) => void;
  focusTerminal: (windowId: string) => void;
}

interface AttentionNavigation {
  goToNextWindowNeedingYou: () => void;
}

interface AttentionNavigationModule {
  nextWindowNeedingYou: <T extends NavigableWindow>(
    groups: readonly RoomGroup<T>[]
  ) => AttentionTarget<T> | null;
  createAttentionNavigation: <T extends NavigableWindow>(
    deps: AttentionNavigationDeps<T>
  ) => AttentionNavigation;
}

const navigationModules = import.meta.glob<AttentionNavigationModule>('./attention-navigation.ts', {
  eager: true,
});

/** The module's exports, or a failure naming the file that could not be read. */
function attentionNavigationModule(): AttentionNavigationModule {
  const found = Object.values(navigationModules)[0];
  if (found === undefined) {
    throw new Error('gui/src/session/attention-navigation.ts could not be read');
  }
  return found;
}

// ---------------------------------------------------------------------------
// Fixture: two rooms whose screen order differs from the global tab order
// ---------------------------------------------------------------------------

interface Entry {
  windowId: string;
  cwd: string;
  room: { roomId: string; name: string } | null;
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
    room: { roomId, name: roomId },
    detected: status === 'unknown' ? { status, reason: null } : { status },
    display: { isMinimized: false, zOrder },
  };
}

/**
 * The four windows of the two-room fixture. `waiting` names the ones whose
 * status makes them need the user; `front` names the one holding the highest
 * `zOrder`, which the jump reads as the starting point of the cycle.
 */
function fourWindows(waiting: readonly string[], front: string): Entry[] {
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
  navigation: AttentionNavigation;
  collapsedRooms: CollapsedRooms;
  /** Every `expand:<roomId>` / `focus:<windowId>` call, in the order they were made. */
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
function harnessOf(entries: readonly Entry[], collapsedRoomIds: readonly string[]): Harness {
  const collapsedRooms = createCollapsedRooms();
  for (const roomId of collapsedRoomIds) {
    collapsedRooms.toggle(roomId);
  }
  const state = { entries: [...entries] };
  const calls: string[] = [];
  const focused: string[] = [];
  let nextZOrder = FRONT_Z_ORDER + 1;

  const { createAttentionNavigation } = attentionNavigationModule();
  const navigation = createAttentionNavigation<Entry>({
    groups: () => groupsOf(state.entries, get(collapsedRooms)),
    expandRoom: (roomId) => {
      calls.push(`expand:${roomId}`);
      collapsedRooms.expand(roomId);
    },
    focusTerminal: (windowId) => {
      calls.push(`terminal:${windowId}`);
    },
    focus: (entry) => {
      calls.push(`focus:${entry.windowId}`);
      focused.push(entry.windowId);
      // Models `WindowActions.focus`: it raises the window to the front, and
      // the front window is what the jump takes as the next press's origin.
      const zOrder = nextZOrder++;
      state.entries = state.entries.map((candidate) =>
        candidate.windowId === entry.windowId
          ? { ...candidate, display: { ...candidate.display, zOrder } }
          : candidate
      );
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

describe('attention-navigation module (the jump lives in session/)', () => {});

describe('nextWindowNeedingYou: the next waiting window in screen order', () => {
  it('reads the list top to bottom, not in tab order: from the front window it picks a1, the window the screen shows next', () => {
    // In tab order the waiting windows read b2, a1 -- the opposite order --
    // so picking b2 here would mean the cycle followed `tabOrder` globally.
    const { nextWindowNeedingYou } = attentionNavigationModule();

    const target = nextWindowNeedingYou(groupsOf(fourWindows(['a1', 'b2'], 'a2')));

    expect(target?.entry.windowId).toBe('a1');
  });

  it('crosses into the next room when the front window is the previous target', () => {
    const { nextWindowNeedingYou } = attentionNavigationModule();

    const target = nextWindowNeedingYou(groupsOf(fourWindows(['a1', 'b2'], 'a1')));

    expect(target?.entry.windowId).toBe('b2');
  });

  it('names the room of the window it returns, so the caller can expand it', () => {
    const { nextWindowNeedingYou } = attentionNavigationModule();
    const groups = groupsOf(fourWindows(['b2'], 'a2'));

    // groups[1] is room-b, whose first entry on screen is b2.
    expect(groups[1]?.entries[0]?.windowId).toBe('b2');

    const target = nextWindowNeedingYou(groups);

    expect(target).toEqual({ entry: groups[1]?.entries[0], roomId: 'room-b' });
  });

  it('wraps to the first waiting window when the front window sits below the last one', () => {
    const { nextWindowNeedingYou } = attentionNavigationModule();

    const target = nextWindowNeedingYou(groupsOf(fourWindows(['a1', 'b2'], 'b1')));

    expect(target?.entry.windowId).toBe('a1');
  });

  it('skips the front window itself when it is waiting, and wraps past the end', () => {
    const { nextWindowNeedingYou } = attentionNavigationModule();

    const target = nextWindowNeedingYou(groupsOf(fourWindows(['a1', 'b2'], 'b2')));

    expect(target?.entry.windowId).toBe('a1');
  });

  it('returns the only waiting window even when it is the front window (jumping back to it is harmless)', () => {
    const { nextWindowNeedingYou } = attentionNavigationModule();

    const target = nextWindowNeedingYou(groupsOf(fourWindows(['a1'], 'a1')));

    expect(target?.entry.windowId).toBe('a1');
  });

  it('returns null when no window is waiting', () => {
    const { nextWindowNeedingYou } = attentionNavigationModule();

    expect(nextWindowNeedingYou(groupsOf(fourWindows([], 'a2')))).toBeNull();
  });

  it('returns null when there is no room at all', () => {
    const { nextWindowNeedingYou } = attentionNavigationModule();

    expect(nextWindowNeedingYou(groupsOf([]))).toBeNull();
  });

  it('reaches a waiting window inside a collapsed room (collapsing hides a room, it does not mute it)', () => {
    const { nextWindowNeedingYou } = attentionNavigationModule();

    const target = nextWindowNeedingYou(groupsOf(fourWindows(['b2'], 'a2'), new Set(['room-b'])));

    expect(target?.entry.windowId).toBe('b2');
  });
});

// ---------------------------------------------------------------------------
// What the jump does, and what it must leave alone
// ---------------------------------------------------------------------------

describe('createAttentionNavigation: expanding and focusing', () => {
  it('expands the target room before focusing the window', () => {
    const harness = harnessOf(fourWindows(['b2'], 'a2'), ['room-a', 'room-b']);

    harness.navigation.goToNextWindowNeedingYou();

    expect(harness.calls).toEqual(['expand:room-b', 'focus:b2', 'terminal:b2']);
    expect(get(harness.collapsedRooms).has('room-b')).toBe(false);
  });

  it('leaves a room that is already expanded expanded (an expand written as a toggle would close it)', () => {
    const harness = harnessOf(fourWindows(['b2'], 'a2'), []);

    harness.navigation.goToNextWindowNeedingYou();

    expect(get(harness.collapsedRooms)).toEqual(new Set());
    expect(harness.focused).toEqual(['b2']);
  });

  it('leaves the collapsed state of every other room alone (nothing the user did not ask for moves)', () => {
    const harness = harnessOf(fourWindows(['a1'], 'a2'), ['room-a', 'room-b']);

    harness.navigation.goToNextWindowNeedingYou();

    expect(get(harness.collapsedRooms)).toEqual(new Set(['room-b']));
  });

  it('does not collapse the room it jumped into afterwards', () => {
    const harness = harnessOf(fourWindows(['a1', 'b2'], 'a2'), ['room-a']);

    harness.navigation.goToNextWindowNeedingYou();
    harness.navigation.goToNextWindowNeedingYou();

    expect(get(harness.collapsedRooms)).toEqual(new Set());
  });

  it('changes neither the room order nor the order within a room', () => {
    const harness = harnessOf(fourWindows(['a1', 'b2'], 'a2'), []);
    const before = orderSnapshot(harness.currentEntries(), get(harness.collapsedRooms));

    harness.navigation.goToNextWindowNeedingYou();

    const after = orderSnapshot(harness.currentEntries(), get(harness.collapsedRooms));
    expect(after).toEqual(before);
    expect(after.flatMap((group) => group.entries)).toEqual(SCREEN_ORDER);
  });

  it('does nothing at all when no window is waiting', () => {
    const harness = harnessOf(fourWindows([], 'a2'), ['room-b']);

    harness.navigation.goToNextWindowNeedingYou();

    expect(harness.calls).toEqual([]);
    expect(get(harness.collapsedRooms)).toEqual(new Set(['room-b']));
  });
});

describe('createAttentionNavigation: repeated presses (no cursor is held)', () => {
  it('does not skip the next waiting window when the one it just jumped to stops waiting', () => {
    // Waiting in screen order: a1, b2, b1. A cursor remembering "I handed out
    // index 0" would read index 1 of the shrunken list (b2, b1) and hand out
    // b1, skipping b2.
    const harness = harnessOf(fourWindows(['a1', 'b2', 'b1'], 'a2'), []);

    harness.navigation.goToNextWindowNeedingYou();
    harness.stopWaiting('a1');
    harness.navigation.goToNextWindowNeedingYou();

    expect(harness.focused).toEqual(['a1', 'b2']);
  });

  it('walks the waiting windows in screen order and wraps back to the first', () => {
    const harness = harnessOf(fourWindows(['a1', 'b2'], 'a2'), []);

    harness.navigation.goToNextWindowNeedingYou();
    harness.navigation.goToNextWindowNeedingYou();
    harness.navigation.goToNextWindowNeedingYou();

    expect(harness.focused).toEqual(['a1', 'b2', 'a1']);
  });
});
