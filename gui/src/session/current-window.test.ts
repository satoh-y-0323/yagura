/**
 * The window keyboard operations act on, derived from what is on screen, and
 * the next / previous window in the sidebar's order.
 *
 * The module is read through `import.meta.glob` so that, while it does not
 * exist, every case below reports its own unmet contract instead of the whole
 * file failing collection.
 */
import { describe, it, expect } from 'vitest';
import { get } from 'svelte/store';

import type { DetectedStatusView } from '../ipc/session-boundary';
import type { WindowInfo } from '../ipc/session-client';
import { groupByRoom, windowsInDisplayOrder } from '../sidebar/room-groups';
import { createWindowRegistry } from '../stores/window-registry';

/** The part of a window entry the derivation reads. */
interface PlacedWindow {
  windowId: string;
  display: { zOrder: number; isMinimized: boolean; isMaximized: boolean };
}

interface CurrentWindowModule {
  isWindowVisible: (
    entry: PlacedWindow,
    activeTabEntry: { windowId: string } | undefined
  ) => boolean;
  currentWindow: <T extends PlacedWindow>(windows: readonly T[]) => T | null;
  neighborWindow: <T extends { windowId: string }>(
    order: readonly T[],
    current: { windowId: string } | null,
    step: 1 | -1
  ) => T | null;
}

const currentWindowModules = import.meta.glob<CurrentWindowModule>('./current-window.ts', {
  eager: true,
});

function currentWindowModule(): CurrentWindowModule {
  const found = Object.values(currentWindowModules)[0];
  if (found === undefined) {
    throw new Error('gui/src/session/current-window.ts could not be read');
  }
  return found;
}

interface Placement {
  z: number;
  minimized?: boolean;
  maximized?: boolean;
}

function placed(windowId: string, placement: Placement): PlacedWindow {
  return {
    windowId,
    display: {
      zOrder: placement.z,
      isMinimized: placement.minimized ?? false,
      isMaximized: placement.maximized ?? false,
    },
  };
}

/** The active tab exactly as the view derives it: the maximized window, if any. */
function activeTabOf(windows: readonly PlacedWindow[]): PlacedWindow | undefined {
  return windows.find((entry) => entry.display.isMaximized);
}

describe('current-window module', () => {});

describe('isWindowVisible: the one definition of a window being on screen', () => {
  it('shows every window that is not minimized while no window is maximized', () => {
    const { isWindowVisible } = currentWindowModule();

    expect(isWindowVisible(placed('a', { z: 1 }), undefined)).toBe(true);
    expect(isWindowVisible(placed('b', { z: 9 }), undefined)).toBe(true);
  });

  it('hides a minimized window', () => {
    const { isWindowVisible } = currentWindowModule();

    expect(isWindowVisible(placed('a', { z: 9, minimized: true }), undefined)).toBe(false);
  });

  it('in tab mode shows the active tab and hides every other window, even one that is not minimized', () => {
    const { isWindowVisible } = currentWindowModule();
    const tab = placed('tab', { z: 1, maximized: true });
    const other = placed('other', { z: 9 });

    expect(isWindowVisible(tab, tab)).toBe(true);
    expect(isWindowVisible(other, tab)).toBe(false);
  });

  it('hides the active tab itself once it is minimized', () => {
    const { isWindowVisible } = currentWindowModule();
    const tab = placed('tab', { z: 1, maximized: true, minimized: true });

    expect(isWindowVisible(tab, tab)).toBe(false);
  });
});

describe('currentWindow: picked from the windows on screen', () => {
  it('picks the window with the highest zOrder when none is maximized', () => {
    const { currentWindow } = currentWindowModule();
    const windows = [placed('a', { z: 3 }), placed('b', { z: 7 }), placed('c', { z: 5 })];

    expect(currentWindow(windows)?.windowId).toBe('b');
  });

  it('skips a minimized window even when it holds the highest zOrder', () => {
    const { currentWindow } = currentWindowModule();
    const windows = [
      placed('a', { z: 3 }),
      placed('b', { z: 90, minimized: true }),
      placed('c', { z: 5 }),
    ];

    expect(currentWindow(windows)?.windowId).toBe('c');
  });

  it('breaks a zOrder tie in favour of the window later in the list (the one opened later)', () => {
    const { currentWindow } = currentWindowModule();

    expect(currentWindow([placed('a', { z: 4 }), placed('b', { z: 4 })])?.windowId).toBe('b');
    expect(currentWindow([placed('b', { z: 4 }), placed('a', { z: 4 })])?.windowId).toBe('a');
  });

  it('picks the maximized window even when its zOrder is the lowest (it renders on top regardless)', () => {
    const { currentWindow } = currentWindowModule();
    const windows = [placed('front', { z: 90 }), placed('tab', { z: 1, maximized: true })];

    expect(currentWindow(windows)?.windowId).toBe('tab');
  });

  it('returns null when the maximized window is minimized: the windows behind the tab are hidden too', () => {
    // Minimizing keeps isMaximized, so neither the minimized tab nor the
    // windows hidden behind it may become the target.
    const { currentWindow } = currentWindowModule();
    const windows = [
      placed('behind', { z: 90 }),
      placed('tab', { z: 1, maximized: true, minimized: true }),
    ];

    expect(currentWindow(windows)).toBeNull();
  });

  it('returns null when there is no window', () => {
    const { currentWindow } = currentWindowModule();

    expect(currentWindow([])).toBeNull();
  });

  it('returns null when every window is minimized', () => {
    const { currentWindow } = currentWindowModule();
    const windows = [
      placed('a', { z: 3, minimized: true }),
      placed('b', { z: 7, minimized: true }),
    ];

    expect(currentWindow(windows)).toBeNull();
  });

  it('agrees with isWindowVisible on every fixture: null exactly when nothing is visible, otherwise a visible window', () => {
    const { currentWindow, isWindowVisible } = currentWindowModule();
    const fixtures: PlacedWindow[][] = [
      [placed('a', { z: 1 }), placed('b', { z: 2 })],
      [placed('a', { z: 5, minimized: true }), placed('b', { z: 2 })],
      [placed('a', { z: 5 }), placed('b', { z: 2, maximized: true })],
      [placed('a', { z: 5 }), placed('b', { z: 2, maximized: true, minimized: true })],
      [placed('a', { z: 5, minimized: true }), placed('b', { z: 2, minimized: true })],
      [placed('a', { z: 1, maximized: true }), placed('b', { z: 9, minimized: true })],
    ];

    for (const windows of fixtures) {
      const activeTab = activeTabOf(windows);
      const visible = windows.filter((entry) => isWindowVisible(entry, activeTab));
      const picked = currentWindow(windows);
      if (visible.length === 0) {
        expect(picked).toBeNull();
      } else {
        expect(picked).not.toBeNull();
        expect(visible).toContain(picked);
      }
    }
  });

  it('follows the registry: maximizing then minimizing a window leaves no current window, and un-maximizing brings the other back', () => {
    const { currentWindow } = currentWindowModule();
    const registry = createWindowRegistry();
    registry.openWindow(windowInfo('w-a'));
    registry.openWindow(windowInfo('w-b'));
    registry.setWindowMaximized('w-a', true);

    expect(currentWindow(get(registry).windows)?.windowId).toBe('w-a');

    registry.setWindowsMinimized(['w-a'], true);

    expect(currentWindow(get(registry).windows)).toBeNull();

    registry.setWindowMaximized('w-a', false);

    expect(currentWindow(get(registry).windows)?.windowId).toBe('w-b');
  });
});

describe('neighborWindow: stepping through the given order', () => {
  const order = [{ windowId: 'a' }, { windowId: 'b' }, { windowId: 'c' }];

  it('steps forward and backward by one', () => {
    const { neighborWindow } = currentWindowModule();

    expect(neighborWindow(order, { windowId: 'b' }, 1)?.windowId).toBe('c');
    expect(neighborWindow(order, { windowId: 'b' }, -1)?.windowId).toBe('a');
  });

  it('wraps around at both ends', () => {
    const { neighborWindow } = currentWindowModule();

    expect(neighborWindow(order, { windowId: 'c' }, 1)?.windowId).toBe('a');
    expect(neighborWindow(order, { windowId: 'a' }, -1)?.windowId).toBe('c');
  });

  it('starts from the first window going forward and the last going backward when there is no current window', () => {
    const { neighborWindow } = currentWindowModule();

    expect(neighborWindow(order, null, 1)?.windowId).toBe('a');
    expect(neighborWindow(order, null, -1)?.windowId).toBe('c');
  });

  it('treats a current window missing from the order like no current window', () => {
    const { neighborWindow } = currentWindowModule();

    expect(neighborWindow(order, { windowId: 'gone' }, 1)?.windowId).toBe('a');
    expect(neighborWindow(order, { windowId: 'gone' }, -1)?.windowId).toBe('c');
  });

  it('returns the only window itself in both directions', () => {
    const { neighborWindow } = currentWindowModule();
    const single = [{ windowId: 'only' }];

    expect(neighborWindow(single, { windowId: 'only' }, 1)?.windowId).toBe('only');
    expect(neighborWindow(single, { windowId: 'only' }, -1)?.windowId).toBe('only');
  });

  it('returns null for an empty order', () => {
    const { neighborWindow } = currentWindowModule();

    expect(neighborWindow([], null, 1)).toBeNull();
    expect(neighborWindow([], { windowId: 'a' }, -1)).toBeNull();
  });

  it('matches the current window by windowId, not by object identity', () => {
    const { neighborWindow } = currentWindowModule();
    const copyOfB = { windowId: 'b' };

    expect(order).not.toContain(copyOfB);
    expect(neighborWindow(order, copyOfB, 1)?.windowId).toBe('c');
  });

  it('returns an entry of the order itself, so the caller acts on the real window', () => {
    const { neighborWindow } = currentWindowModule();

    expect(neighborWindow(order, { windowId: 'a' }, 1)).toBe(order[1]);
  });
});

interface SidebarEntry extends PlacedWindow {
  cwd: string;
  room: { roomId: string; name: string } | null;
  detected: DetectedStatusView;
}

function sidebarEntry(windowId: string, roomId: string, placement: Placement): SidebarEntry {
  return {
    ...placed(windowId, placement),
    cwd: `C:\\Users\\dev\\${roomId}`,
    room: { roomId, name: roomId },
    detected: { status: 'running' },
  };
}

describe('neighborWindow over the sidebar order: collapsed rooms and minimized windows are stops too', () => {
  const entries = [
    sidebarEntry('a1', 'room-a', { z: 5 }),
    sidebarEntry('a2', 'room-a', { z: 1, minimized: true }),
    sidebarEntry('b1', 'room-b', { z: 1 }),
  ];
  const displayOrder = windowsInDisplayOrder(
    groupByRoom(entries, new Set(['room-b']), ['a1', 'a2', 'b1'])
  );

  it('steps from the current window onto a minimized window next in the sidebar', () => {
    const { currentWindow, neighborWindow } = currentWindowModule();

    const next = neighborWindow(displayOrder, currentWindow(entries), 1);

    expect(next?.windowId).toBe('a2');
  });

  it('steps into a window of a collapsed room', () => {
    const { neighborWindow } = currentWindowModule();

    expect(neighborWindow(displayOrder, { windowId: 'a2' }, 1)?.windowId).toBe('b1');
    expect(neighborWindow(displayOrder, { windowId: 'a1' }, -1)?.windowId).toBe('b1');
  });
});

function windowInfo(windowId: string): WindowInfo {
  return {
    window_id: windowId,
    name: windowId,
    vendor: null,
    cwd: 'C:\\Users\\dev\\project',
    detected: { status: 'running' },
    exit_code: null,
  };
}
