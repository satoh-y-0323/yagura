/**
 * The "next window that needs you" jump starts from the current window it is
 * given, so it and the keyboard operations can never disagree about where the
 * user is. How the current window is derived is measured in
 * `current-window.test.ts`.
 */
import { describe, it, expect } from 'vitest';
import { get } from 'svelte/store';

import type { DetectedStatusView, WindowStatus } from '../ipc/session-boundary';
import { createCollapsedRooms } from '../sidebar/collapsed-rooms';
import { groupByRoom } from '../sidebar/room-groups';
import { createAttentionNavigation, nextWindowNeedingYou } from './attention-navigation';

interface Entry {
  windowId: string;
  cwd: string;
  room: { roomId: string; name: string } | null;
  detected: DetectedStatusView;
  display: { zOrder: number; isMinimized: boolean; isMaximized: boolean };
}

interface Placement {
  z: number;
  waiting?: boolean;
}

/** The screen order is the order below: room-a (a1, a2), then room-b (b1, b2). */
const TAB_ORDER = ['a1', 'a2', 'b1', 'b2'];

function entry(windowId: string, placement: Placement): Entry {
  const roomId = windowId.startsWith('a') ? 'room-a' : 'room-b';
  const status: WindowStatus = placement.waiting === true ? 'waiting_input' : 'running';
  return {
    windowId,
    cwd: `C:\\Users\\dev\\${roomId}`,
    room: { roomId, name: roomId },
    detected: { status },
    display: {
      zOrder: placement.z,
      isMinimized: false,
      isMaximized: false,
    },
  };
}

function groupsOf(entries: readonly Entry[], collapsed: ReadonlySet<string> = new Set()) {
  return groupByRoom(entries, collapsed, TAB_ORDER);
}

describe('nextWindowNeedingYou starts from the current window', () => {
  it('starts from the top of the list when there is no current window, whatever the placement', () => {
    // a2 is in front on the canvas; starting from it would pick b2.
    const entries = [
      entry('a1', { z: 1 }),
      entry('a2', { z: 90, waiting: true }),
      entry('b1', { z: 1 }),
      entry('b2', { z: 1, waiting: true }),
    ];

    expect(nextWindowNeedingYou(groupsOf(entries), null)?.entry.windowId).toBe('a2');
  });
});

describe('goToNextWindowNeedingYou reports where it went', () => {
  function navigationOver(
    entries: readonly Entry[],
    collapsedRoomIds: readonly string[],
    current: { windowId: string } | null
  ) {
    const collapsedRooms = createCollapsedRooms();
    for (const roomId of collapsedRoomIds) {
      collapsedRooms.toggle(roomId);
    }
    const calls: string[] = [];
    const navigation = createAttentionNavigation<Entry>({
      groups: () => groupsOf(entries, get(collapsedRooms)),
      current: () => current,
      expandRoom: (roomId) => {
        calls.push(`expand:${roomId}`);
        collapsedRooms.expand(roomId);
      },
      focusTerminal: (windowId) => {
        calls.push(`terminal:${windowId}`);
      },
      goTo: (target) => {
        calls.push(`goTo:${target.windowId}`);
      },
    });
    return { navigation, calls, collapsedRooms };
  }

  it('returns the window it focused, after expanding its room and then focusing it', () => {
    const entries = [
      entry('a1', { z: 5 }),
      entry('a2', { z: 1 }),
      entry('b1', { z: 1 }),
      entry('b2', { z: 1, waiting: true }),
    ];
    const { navigation, calls, collapsedRooms } = navigationOver(entries, ['room-b'], {
      windowId: 'a1',
    });

    const landed: Entry | null = navigation.goToNextWindowNeedingYou();

    expect(landed).toBe(entries[3]);
    expect(calls).toEqual(['expand:room-b', 'goTo:b2', 'terminal:b2']);
    expect(get(collapsedRooms).has('room-b')).toBe(false);
  });

  it('returns null and touches nothing when no window is waiting', () => {
    const entries = [entry('a1', { z: 5 }), entry('b1', { z: 1 })];
    const { navigation, calls, collapsedRooms } = navigationOver(entries, ['room-b'], {
      windowId: 'a1',
    });

    const landed: Entry | null = navigation.goToNextWindowNeedingYou();

    expect(landed).toBeNull();
    expect(calls).toEqual([]);
    expect(get(collapsedRooms)).toEqual(new Set(['room-b']));
  });
});
