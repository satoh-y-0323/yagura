/**
 * `arrangeWindows` tiles every open window into an equal-size grid in tab order,
 * restoring any minimized or maximized window in the same update. The grid
 * arithmetic itself is covered by `../layout/tile-layout.test.ts`.
 */
import { describe, it, expect, vi } from 'vitest';
import { get } from 'svelte/store';

import { createWindowRegistry } from './window-registry';
import type { WindowDisplay, WindowRegistry } from './window-registry';
import type { WindowInfo } from '../ipc/session-client';
import { MIN_WINDOW_SIZE, type Rect, type Size } from '../layout/float-layout';
import { tileRects } from '../layout/tile-layout';

// Partial mock: every case but the fail-safe one below must keep exercising the
// real grid arithmetic.
vi.mock('../layout/tile-layout', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../layout/tile-layout')>();
  return { ...actual, tileRects: vi.fn(actual.tileRects) };
});

function makeWindowInfo(windowId: string): WindowInfo {
  return {
    window_id: windowId,
    name: windowId,
    vendor: null,
    cwd: 'C:\\Users\\dev\\project',
    detected: { status: 'running' },
    exit_code: null,
  };
}

function openRegistry(...windowIds: string[]): WindowRegistry {
  const registry = createWindowRegistry();
  for (const windowId of windowIds) {
    registry.openWindow(makeWindowInfo(windowId));
  }
  return registry;
}

function displayOf(registry: WindowRegistry, windowId: string): WindowDisplay {
  const entry = get(registry).windows.find((candidate) => candidate.windowId === windowId);
  if (entry === undefined) {
    throw new Error(`window ${windowId} is not open`);
  }
  return entry.display;
}

function zOrdersOf(registry: WindowRegistry): Map<string, number> {
  return new Map(get(registry).windows.map((entry) => [entry.windowId, entry.display.zOrder]));
}

/**
 * The tab reorder at the end makes `tabOrder` differ from the order the windows
 * were opened in: `arrangeWindows` must fill the grid in `tabOrder`.
 */
function mixedFixture(): WindowRegistry {
  const registry = openRegistry('w-a', 'w-b', 'w-c', 'w-d');
  registry.setWindowMinimized('w-a', true);
  registry.setWindowMinimized('w-b', true);
  registry.setWindowPosition('w-c', { x: 10, y: 10 });
  registry.setWindowSize('w-c', { width: 400, height: 300 });
  registry.setWindowMaximized('w-c', true);
  // w-d stays a normal, un-minimized, un-maximized window.
  registry.moveTab('w-d', 0);
  return registry;
}

const AREA: Size = { width: 1000, height: 700 };
/** cols=2, rows=2 for 4 windows; floor(1000/2) x floor(700/2). */
const CELL: Size = { width: 500, height: 350 };

describe('arrangeWindows: restores every window', () => {
  it('clears isMinimized, isMaximized and restoreBounds on every window in one call', () => {
    const registry = mixedFixture();

    registry.arrangeWindows(AREA);

    for (const windowId of ['w-a', 'w-b', 'w-c', 'w-d']) {
      const display = displayOf(registry, windowId);
      expect({
        isMinimized: display.isMinimized,
        isMaximized: display.isMaximized,
        restoreBounds: display.restoreBounds,
      }).toEqual({ isMinimized: false, isMaximized: false, restoreBounds: null });
    }
  });
});

describe('arrangeWindows: tiles windows in tabOrder, not windows-array order', () => {
  it('has a tabOrder that already differs from the windows-array order', () => {
    const registry = mixedFixture();

    expect(get(registry).windows.map((entry) => entry.windowId)).toEqual([
      'w-a',
      'w-b',
      'w-c',
      'w-d',
    ]);
    expect(get(registry).tabOrder).toEqual(['w-d', 'w-a', 'w-b', 'w-c']);
  });

  it('places every window at the grid cell matching its tabOrder position', () => {
    const registry = mixedFixture();

    registry.arrangeWindows(AREA);

    expect(displayOf(registry, 'w-d').position).toEqual({ x: 0, y: 0 });
    expect(displayOf(registry, 'w-d').size).toEqual(CELL);
    expect(displayOf(registry, 'w-a').position).toEqual({ x: 500, y: 0 });
    expect(displayOf(registry, 'w-a').size).toEqual(CELL);
    expect(displayOf(registry, 'w-b').position).toEqual({ x: 0, y: 350 });
    expect(displayOf(registry, 'w-b').size).toEqual(CELL);
    expect(displayOf(registry, 'w-c').position).toEqual({ x: 500, y: 350 });
    expect(displayOf(registry, 'w-c').size).toEqual(CELL);
  });
});

describe('arrangeWindows: notification contract', () => {
  it('notifies subscribers exactly once per arrange call', () => {
    const registry = mixedFixture();
    const listener = vi.fn();
    registry.subscribe(listener);
    listener.mockClear();

    registry.arrangeWindows(AREA);

    expect(listener).toHaveBeenCalledTimes(1);
  });
});

describe('arrangeWindows: no-op below the minimum area', () => {
  it('does not update the store when the area width is one pixel below the minimum', () => {
    const registry = mixedFixture();
    const before = get(registry);
    const listener = vi.fn();
    registry.subscribe(listener);
    listener.mockClear();

    registry.arrangeWindows({ width: MIN_WINDOW_SIZE.width - 1, height: 700 });

    expect(listener).not.toHaveBeenCalled();
    expect(get(registry)).toEqual(before);
  });

  it('does not update the store when the area height is one pixel below the minimum', () => {
    const registry = mixedFixture();
    const before = get(registry);
    const listener = vi.fn();
    registry.subscribe(listener);
    listener.mockClear();

    registry.arrangeWindows({ width: 1000, height: MIN_WINDOW_SIZE.height - 1 });

    expect(listener).not.toHaveBeenCalled();
    expect(get(registry)).toEqual(before);
  });

  it('does arrange when the area is exactly the minimum size', () => {
    const registry = mixedFixture();
    const listener = vi.fn();
    registry.subscribe(listener);
    listener.mockClear();

    registry.arrangeWindows(MIN_WINDOW_SIZE);

    expect(listener).toHaveBeenCalledTimes(1);
    expect(displayOf(registry, 'w-a').isMinimized).toBe(false);
  });
});

/**
 * `tileRects` can return fewer rectangles than there are windows to place, and that
 * mismatch must be a no-op: moving only the covered windows would leave the rest at
 * their old geometry.
 */
describe('arrangeWindows: fail-safe when tileRects returns fewer rectangles than windows', () => {
  it('leaves the registry state unchanged when tileRects returns fewer rectangles than open windows', () => {
    const registry = mixedFixture();
    const before = get(registry);
    vi.mocked(tileRects).mockReturnValueOnce([
      { position: { x: 0, y: 0 }, size: { width: 500, height: 350 } },
    ]);

    registry.arrangeWindows(AREA);

    expect(get(registry)).toEqual(before);
  });
});

describe('arrangeWindows: no-op with zero windows', () => {
  it('leaves an empty registry state unchanged', () => {
    const registry = createWindowRegistry();
    const before = get(registry);

    registry.arrangeWindows(AREA);

    expect(get(registry)).toEqual(before);
  });
});

describe('arrangeWindows: leaves zOrder untouched', () => {
  it('keeps the stacking order unchanged', () => {
    const registry = mixedFixture();
    registry.bringWindowToFront('w-b');
    const zOrdersBefore = zOrdersOf(registry);

    registry.arrangeWindows(AREA);

    expect(zOrdersOf(registry)).toEqual(zOrdersBefore);
  });
});

describe('arrangeWindows: is not an automatic re-layout', () => {
  it('leaves the already-arranged windows geometry unchanged when a new window opens afterwards', () => {
    const registry = mixedFixture();
    registry.arrangeWindows(AREA);
    const before = {
      'w-a': displayOf(registry, 'w-a'),
      'w-b': displayOf(registry, 'w-b'),
      'w-c': displayOf(registry, 'w-c'),
      'w-d': displayOf(registry, 'w-d'),
    };

    registry.openWindow(makeWindowInfo('w-e'));

    expect(displayOf(registry, 'w-a')).toEqual(before['w-a']);
    expect(displayOf(registry, 'w-b')).toEqual(before['w-b']);
    expect(displayOf(registry, 'w-c')).toEqual(before['w-c']);
    expect(displayOf(registry, 'w-d')).toEqual(before['w-d']);
  });
});

// ---------------------------------------------------------------------------
// `arrangeWindowsOf` tiles one explicit set of windows -- the windows of one
// room -- into the same grid, and collapses everything outside the set so the
// workspace shows the set alone.
// ---------------------------------------------------------------------------

function arrangeWindowsOf(
  registry: WindowRegistry,
  area: Size,
  windowIds: readonly string[]
): void {
  registry.arrangeWindowsOf(area, windowIds);
}

/** The cell at `index` of the grid `count` windows tile `AREA` into. */
function cellOf(count: number, index: number): Rect {
  const rect = tileRects(AREA, count)[index];
  if (rect === undefined) {
    throw new Error(`no cell ${index} in a grid of ${count}`);
  }
  return rect;
}

describe('arrangeWindowsOf: tiles the target set and collapses the rest', () => {
  it('places the targets at the cells matching their tabOrder positions', () => {
    const registry = mixedFixture();
    const first = cellOf(2, 0);
    const second = cellOf(2, 1);

    // Passed in the reverse of the tab order, which is ['w-d', 'w-a', 'w-b', 'w-c'].
    arrangeWindowsOf(registry, AREA, ['w-c', 'w-a']);

    expect(displayOf(registry, 'w-a').position).toEqual(first.position);
    expect(displayOf(registry, 'w-a').size).toEqual(first.size);
    expect(displayOf(registry, 'w-c').position).toEqual(second.position);
    expect(displayOf(registry, 'w-c').size).toEqual(second.size);
  });

  it('minimizes every window outside the target set', () => {
    const registry = mixedFixture();

    arrangeWindowsOf(registry, AREA, ['w-c', 'w-a']);

    expect(displayOf(registry, 'w-b').isMinimized).toBe(true);
    expect(displayOf(registry, 'w-d').isMinimized).toBe(true);
  });

  it('brings a minimized target back into the grid', () => {
    const registry = mixedFixture();
    expect(displayOf(registry, 'w-a').isMinimized).toBe(true);

    arrangeWindowsOf(registry, AREA, ['w-c', 'w-a']);

    expect(displayOf(registry, 'w-a').isMinimized).toBe(false);
  });

  it('clears isMaximized and restoreBounds on the targets and on the rest alike', () => {
    const registry = mixedFixture();

    arrangeWindowsOf(registry, AREA, ['w-c', 'w-a']);

    for (const windowId of ['w-a', 'w-b', 'w-c', 'w-d']) {
      const display = displayOf(registry, windowId);
      expect({ isMaximized: display.isMaximized, restoreBounds: display.restoreBounds }).toEqual({
        isMaximized: false,
        restoreBounds: null,
      });
    }
  });

  it('notifies subscribers exactly once', () => {
    const registry = mixedFixture();
    const listener = vi.fn();
    registry.subscribe(listener);
    listener.mockClear();

    arrangeWindowsOf(registry, AREA, ['w-c', 'w-a']);

    expect(listener).toHaveBeenCalledTimes(1);
  });
});

describe('arrangeWindowsOf: refuses a set it cannot place in full', () => {
  it('leaves the state unchanged when tileRects returns a cell count other than the target count', () => {
    const registry = mixedFixture();
    const before = get(registry);
    vi.mocked(tileRects).mockReturnValueOnce([
      { position: { x: 0, y: 0 }, size: { width: 500, height: 350 } },
      { position: { x: 500, y: 0 }, size: { width: 500, height: 350 } },
      { position: { x: 0, y: 350 }, size: { width: 500, height: 350 } },
    ]);

    arrangeWindowsOf(registry, AREA, ['w-c', 'w-a']);

    expect(get(registry)).toEqual(before);
  });

  it('leaves the state unchanged when a target is not an open window', () => {
    const registry = mixedFixture();
    const before = get(registry);

    arrangeWindowsOf(registry, AREA, ['w-a', 'w-missing']);

    expect(get(registry)).toEqual(before);
  });

  it('does not update the store when the area is below the minimum', () => {
    const registry = mixedFixture();
    const before = get(registry);
    const listener = vi.fn();
    registry.subscribe(listener);
    listener.mockClear();

    arrangeWindowsOf(registry, { width: MIN_WINDOW_SIZE.width - 1, height: 700 }, ['w-a', 'w-c']);

    expect(listener).not.toHaveBeenCalled();
    expect(get(registry)).toEqual(before);
  });

  it('leaves the state unchanged for an empty target set, minimizing nothing', () => {
    const registry = mixedFixture();
    const before = get(registry);

    arrangeWindowsOf(registry, AREA, []);

    expect(get(registry)).toEqual(before);
  });
});

describe('arrangeWindowsOf: the whole tab order is what arrangeWindows does', () => {
  it('produces the same state as arrangeWindows from the same state', () => {
    const wholeSet = mixedFixture();
    const everyWindow = mixedFixture();

    arrangeWindowsOf(wholeSet, AREA, get(wholeSet).tabOrder);
    everyWindow.arrangeWindows(AREA);

    expect(get(wholeSet)).toEqual(get(everyWindow));
  });
});
