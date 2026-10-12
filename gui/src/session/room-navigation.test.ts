/**
 * The one place that knows which room a window sits in and how a window is
 * brought on screen: expand its room, bring it to the front, focus its
 * terminal. The shortcut runner and the "next window needing you" jump both go
 * through it, so the two cannot drift apart.
 *
 * The module is read through `import.meta.glob`, so a missing module fails the
 * tests that need it instead of the collection of the whole file.
 */
import { describe, it, expect } from 'vitest';

import type { RoomGroup } from '../sidebar/room-groups';

// ---------------------------------------------------------------------------
// The contract under test
// ---------------------------------------------------------------------------

interface Located {
  windowId: string;
}

/** What bringing a window on screen acts through. */
interface RevealDeps<T> {
  /** The sidebar's rooms as they are currently rendered. */
  groups: () => readonly RoomGroup<T>[];
  /** Opens a room without closing one that is already open. */
  expandRoom: (roomId: string) => void;
  /** Brings a window to the front, un-minimizing or switching tabs as needed. */
  focus: (entry: T) => void;
  /** Asks for keyboard focus to move into a window's terminal. */
  focusTerminal: (windowId: string) => void;
}

interface RoomNavigationModule {
  roomOfWindow: <T extends Located>(groups: readonly RoomGroup<T>[], entry: T) => string | null;
  revealWindow: <T extends Located>(deps: RevealDeps<T>, entry: T) => void;
}

const modules = import.meta.glob<RoomNavigationModule>('./room-navigation.ts', { eager: true });

function roomNavigation(): RoomNavigationModule {
  const found = Object.values(modules)[0];
  if (found === undefined) {
    throw new Error('gui/src/session/room-navigation.ts could not be read');
  }
  return found;
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

interface Win {
  windowId: string;
}

function group(roomId: string, entries: Win[]): RoomGroup<Win> {
  return { roomId, name: roomId, entries, attention: 0, unknown: 0, minimized: 0, collapsed: true };
}

const A1: Win = { windowId: 'a1' };
const B1: Win = { windowId: 'b1' };
const B2: Win = { windowId: 'b2' };
const GROUPS: RoomGroup<Win>[] = [group('room-a', [A1]), group('room-b', [B1, B2])];

function recorder(groups: RoomGroup<Win>[]): { deps: RevealDeps<Win>; calls: string[] } {
  const calls: string[] = [];
  return {
    calls,
    deps: {
      groups: () => groups,
      expandRoom: (roomId) => calls.push(`expand:${roomId}`),
      focus: (entry) => calls.push(`focus:${entry.windowId}`),
      focusTerminal: (windowId) => calls.push(`terminal:${windowId}`),
    },
  };
}

// ---------------------------------------------------------------------------
// Behaviour
// ---------------------------------------------------------------------------

describe('roomOfWindow', () => {
  it('names the room whose entries hold the window, wherever the room sits in the list', () => {
    const { roomOfWindow } = roomNavigation();
    expect(roomOfWindow(GROUPS, A1)).toBe('room-a');
    expect(roomOfWindow(GROUPS, B2)).toBe('room-b');
  });

  it('finds the window by its id, so an entry the registry has replaced still resolves', () => {
    const { roomOfWindow } = roomNavigation();
    expect(roomOfWindow(GROUPS, { windowId: 'b1' })).toBe('room-b');
  });

  it('answers null for a window no room holds', () => {
    const { roomOfWindow } = roomNavigation();
    expect(roomOfWindow(GROUPS, { windowId: 'gone' })).toBeNull();
    expect(roomOfWindow([], A1)).toBeNull();
  });
});

describe('revealWindow', () => {
  it('expands the window’s own room first, then brings it to the front and focuses its terminal', () => {
    const { revealWindow } = roomNavigation();
    const { deps, calls } = recorder(GROUPS);
    revealWindow(deps, B2);
    expect(calls).toEqual(['expand:room-b', 'focus:b2', 'terminal:b2']);
  });

  it('reads the rooms at the time of the call, not when the dependencies were built', () => {
    const { revealWindow } = roomNavigation();
    const live: RoomGroup<Win>[] = [group('room-a', [A1])];
    const { deps, calls } = recorder(live);
    live.push(group('room-c', [B1]));
    revealWindow(deps, B1);
    expect(calls).toEqual(['expand:room-c', 'focus:b1', 'terminal:b1']);
  });

  it('still brings forward a window no room holds, without expanding anything', () => {
    const { revealWindow } = roomNavigation();
    const { deps, calls } = recorder(GROUPS);
    revealWindow(deps, { windowId: 'loose' });
    expect(calls).toEqual(['focus:loose', 'terminal:loose']);
  });
});
