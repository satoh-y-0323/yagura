/**
 * The window keyboard operations act on, derived from the work view (the
 * canvas placement, or the window Focus shows), and the next / previous window
 * in the sidebar's order.
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

type WorkView = { mode: 'canvas' } | { mode: 'focus'; focusedWindowId: string | null };

interface CurrentWindowModule {
  isWindowVisibleFor: <T extends PlacedWindow>(
    view: WorkView,
    entry: T,
    windows: readonly T[]
  ) => boolean;
  currentWindowFor: <T extends PlacedWindow>(view: WorkView, windows: readonly T[]) => T | null;
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

const CANVAS: WorkView = { mode: 'canvas' };

function focusOn(focusedWindowId: string | null): WorkView {
  return { mode: 'focus', focusedWindowId };
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

describe('current-window module', () => {});

describe('isWindowVisibleFor in Canvas: the one definition of a window being on screen', () => {
  it('shows every window that is not minimized while no window is maximized', () => {
    const { isWindowVisibleFor } = currentWindowModule();
    const a = placed('a', { z: 1 });
    const b = placed('b', { z: 9 });

    expect(isWindowVisibleFor(CANVAS, a, [a, b])).toBe(true);
    expect(isWindowVisibleFor(CANVAS, b, [a, b])).toBe(true);
  });

  it('hides a minimized window', () => {
    const { isWindowVisibleFor } = currentWindowModule();
    const minimized = placed('a', { z: 9, minimized: true });

    expect(isWindowVisibleFor(CANVAS, minimized, [minimized, placed('b', { z: 1 })])).toBe(false);
  });

  it('shows a maximized window and hides every other window behind it, even one that is not minimized', () => {
    const { isWindowVisibleFor } = currentWindowModule();
    const maximized = placed('max', { z: 1, maximized: true });
    const other = placed('other', { z: 9 });

    expect(isWindowVisibleFor(CANVAS, maximized, [maximized, other])).toBe(true);
    expect(isWindowVisibleFor(CANVAS, other, [maximized, other])).toBe(false);
  });

  it('hides a maximized window itself once it is minimized', () => {
    const { isWindowVisibleFor } = currentWindowModule();
    const maximized = placed('max', { z: 1, maximized: true, minimized: true });

    expect(isWindowVisibleFor(CANVAS, maximized, [maximized])).toBe(false);
  });
});

describe('currentWindowFor in Canvas: picked from the windows on screen', () => {
  it('picks the window with the highest zOrder when none is maximized', () => {
    const { currentWindowFor } = currentWindowModule();
    const windows = [placed('a', { z: 3 }), placed('b', { z: 7 }), placed('c', { z: 5 })];

    expect(currentWindowFor(CANVAS, windows)?.windowId).toBe('b');
  });

  it('skips a minimized window even when it holds the highest zOrder', () => {
    const { currentWindowFor } = currentWindowModule();
    const windows = [
      placed('a', { z: 3 }),
      placed('b', { z: 90, minimized: true }),
      placed('c', { z: 5 }),
    ];

    expect(currentWindowFor(CANVAS, windows)?.windowId).toBe('c');
  });

  it('breaks a zOrder tie in favour of the window later in the list (the one opened later)', () => {
    const { currentWindowFor } = currentWindowModule();
    const ab = [placed('a', { z: 4 }), placed('b', { z: 4 })];
    const ba = [placed('b', { z: 4 }), placed('a', { z: 4 })];

    expect(currentWindowFor(CANVAS, ab)?.windowId).toBe('b');
    expect(currentWindowFor(CANVAS, ba)?.windowId).toBe('a');
  });

  it('picks the maximized window even when its zOrder is the lowest (it renders on top regardless)', () => {
    const { currentWindowFor } = currentWindowModule();
    const windows = [placed('front', { z: 90 }), placed('max', { z: 1, maximized: true })];

    expect(currentWindowFor(CANVAS, windows)?.windowId).toBe('max');
  });

  it('returns null when the maximized window is minimized: the windows behind it are hidden too', () => {
    // Minimizing keeps isMaximized, so neither the minimized window nor the
    // windows hidden behind it may become the target.
    const { currentWindowFor } = currentWindowModule();
    const windows = [
      placed('behind', { z: 90 }),
      placed('max', { z: 1, maximized: true, minimized: true }),
    ];

    expect(currentWindowFor(CANVAS, windows)).toBeNull();
  });

  it('returns null when there is no window', () => {
    const { currentWindowFor } = currentWindowModule();

    expect(currentWindowFor(CANVAS, [])).toBeNull();
  });

  it('returns null when every window is minimized', () => {
    const { currentWindowFor } = currentWindowModule();
    const windows = [
      placed('a', { z: 3, minimized: true }),
      placed('b', { z: 7, minimized: true }),
    ];

    expect(currentWindowFor(CANVAS, windows)).toBeNull();
  });

  it('agrees with isWindowVisibleFor on every fixture: null exactly when nothing is visible, otherwise a visible window', () => {
    const { currentWindowFor, isWindowVisibleFor } = currentWindowModule();
    const fixtures: PlacedWindow[][] = [
      [placed('a', { z: 1 }), placed('b', { z: 2 })],
      [placed('a', { z: 5, minimized: true }), placed('b', { z: 2 })],
      [placed('a', { z: 5 }), placed('b', { z: 2, maximized: true })],
      [placed('a', { z: 5 }), placed('b', { z: 2, maximized: true, minimized: true })],
      [placed('a', { z: 5, minimized: true }), placed('b', { z: 2, minimized: true })],
      [placed('a', { z: 1, maximized: true }), placed('b', { z: 9, minimized: true })],
    ];

    for (const windows of fixtures) {
      const visible = windows.filter((entry) => isWindowVisibleFor(CANVAS, entry, windows));
      const picked = currentWindowFor(CANVAS, windows);
      if (visible.length === 0) {
        expect(picked).toBeNull();
      } else {
        expect(picked).not.toBeNull();
        expect(visible).toContain(picked);
      }
    }
  });

  it('follows the registry: maximizing then minimizing a window leaves no current window, and un-maximizing brings the other back', () => {
    const { currentWindowFor } = currentWindowModule();
    const registry = createWindowRegistry();
    registry.openWindow(windowInfo('w-a'));
    registry.openWindow(windowInfo('w-b'));
    registry.setWindowMaximized('w-a', true);

    expect(currentWindowFor(CANVAS, get(registry).windows)?.windowId).toBe('w-a');

    registry.setWindowsMinimized(['w-a'], true);

    expect(currentWindowFor(CANVAS, get(registry).windows)).toBeNull();

    registry.setWindowMaximized('w-a', false);

    expect(currentWindowFor(CANVAS, get(registry).windows)?.windowId).toBe('w-b');
  });
});

describe('currentWindowFor and isWindowVisibleFor in Focus: the window Focus shows, whatever the placement', () => {
  // On the canvas, `max` would be current and would hide everything behind it.
  const front = placed('front', { z: 90 });
  const chosen = placed('chosen', { z: 1 });
  const max = placed('max', { z: 5, maximized: true });
  const windows = [front, chosen, max];

  it('makes the chosen window current even when another is maximized or in front', () => {
    const { currentWindowFor } = currentWindowModule();

    expect(currentWindowFor(focusOn('chosen'), windows)).toBe(chosen);
  });

  it('shows the chosen window and hides every other one, the maximized one included', () => {
    const { isWindowVisibleFor } = currentWindowModule();
    const view = focusOn('chosen');

    expect(windows.map((entry) => isWindowVisibleFor(view, entry, windows))).toEqual([
      false,
      true,
      false,
    ]);
  });

  it('shows a window minimized on the canvas (Focus has no minimize, so the mark would strand it)', () => {
    const { currentWindowFor, isWindowVisibleFor } = currentWindowModule();
    const hidden = placed('hidden', { z: 1, minimized: true });
    const others = [placed('a', { z: 9 }), hidden];
    const view = focusOn('hidden');

    expect(currentWindowFor(view, others)).toBe(hidden);
    expect(isWindowVisibleFor(view, hidden, others)).toBe(true);
  });

  it('has no current window and shows none when the chosen window is not in the registry', () => {
    const { currentWindowFor, isWindowVisibleFor } = currentWindowModule();
    const view = focusOn('gone');

    expect(currentWindowFor(view, windows)).toBeNull();
    expect(windows.some((entry) => isWindowVisibleFor(view, entry, windows))).toBe(false);
  });

  it('has no current window and shows none when no window is chosen', () => {
    const { currentWindowFor, isWindowVisibleFor } = currentWindowModule();
    const view = focusOn(null);

    expect(currentWindowFor(view, windows)).toBeNull();
    expect(windows.some((entry) => isWindowVisibleFor(view, entry, windows))).toBe(false);
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
    const { currentWindowFor, neighborWindow } = currentWindowModule();

    const next = neighborWindow(displayOrder, currentWindowFor(CANVAS, entries), 1);

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
