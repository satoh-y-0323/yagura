/**
 * The "next window that needs you" jump starts from the window keyboard
 * operations act on, so the two can never disagree about where the user is.
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
  minimized?: boolean;
  maximized?: boolean;
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
      isMinimized: placement.minimized ?? false,
      isMaximized: placement.maximized ?? false,
    },
  };
}

function groupsOf(entries: readonly Entry[], collapsed: ReadonlySet<string> = new Set()) {
  return groupByRoom(entries, collapsed, TAB_ORDER);
}

describe('nextWindowNeedingYou starts from the current window', () => {
  it('does not start from a minimized window, even one holding the highest zOrder', () => {
    // Current is b1; starting from the minimized a2 instead would pick b1 itself.
    const entries = [
      entry('a1', { z: 1, waiting: true }),
      entry('a2', { z: 90, minimized: true }),
      entry('b1', { z: 5, waiting: true }),
      entry('b2', { z: 1 }),
    ];

    expect(nextWindowNeedingYou(groupsOf(entries))?.entry.windowId).toBe('a1');
  });

  it('starts from the maximized window even when a hidden window holds a higher zOrder', () => {
    // Current is the tab b1; starting from the higher-zOrder a2 would pick b1.
    const entries = [
      entry('a1', { z: 1 }),
      entry('a2', { z: 90 }),
      entry('b1', { z: 1, maximized: true, waiting: true }),
      entry('b2', { z: 1, waiting: true }),
    ];

    expect(nextWindowNeedingYou(groupsOf(entries))?.entry.windowId).toBe('b2');
  });

  it('starts from the top of the list when no window is on screen', () => {
    const entries = [
      entry('a1', { z: 1, minimized: true }),
      entry('a2', { z: 1, minimized: true, waiting: true }),
      entry('b1', { z: 90, minimized: true }),
      entry('b2', { z: 1, minimized: true, waiting: true }),
    ];

    expect(nextWindowNeedingYou(groupsOf(entries))?.entry.windowId).toBe('a2');
  });
});

describe('goToNextWindowNeedingYou reports where it went', () => {
  function navigationOver(entries: readonly Entry[], collapsedRoomIds: readonly string[]) {
    const collapsedRooms = createCollapsedRooms();
    for (const roomId of collapsedRoomIds) {
      collapsedRooms.toggle(roomId);
    }
    const calls: string[] = [];
    const navigation = createAttentionNavigation<Entry>({
      groups: () => groupsOf(entries, get(collapsedRooms)),
      expandRoom: (roomId) => {
        calls.push(`expand:${roomId}`);
        collapsedRooms.expand(roomId);
      },
      focusTerminal: (windowId) => {
        calls.push(`terminal:${windowId}`);
      },
      focus: (target) => {
        calls.push(`focus:${target.windowId}`);
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
    const { navigation, calls, collapsedRooms } = navigationOver(entries, ['room-b']);

    const landed: Entry | null = navigation.goToNextWindowNeedingYou();

    expect(landed).toBe(entries[3]);
    expect(calls).toEqual(['expand:room-b', 'focus:b2', 'terminal:b2']);
    expect(get(collapsedRooms).has('room-b')).toBe(false);
  });

  it('returns null and touches nothing when no window is waiting', () => {
    const entries = [entry('a1', { z: 5 }), entry('b1', { z: 1 })];
    const { navigation, calls, collapsedRooms } = navigationOver(entries, ['room-b']);

    const landed: Entry | null = navigation.goToNextWindowNeedingYou();

    expect(landed).toBeNull();
    expect(calls).toEqual([]);
    expect(get(collapsedRooms)).toEqual(new Set(['room-b']));
  });
});
