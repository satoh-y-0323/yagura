/**
 * Layout state carried by the window registry. The delegation cases re-import the
 * store with `../layout/float-layout` replaced, so they use the lazy `vi.doMock`
 * rather than the hoisted `vi.mock`: every other case must run against the real
 * layout module, otherwise the stacking-order assertions would measure a stub.
 */
import { describe, it, expect, vi, afterEach } from 'vitest';
import { get } from 'svelte/store';

import { createWindowRegistry } from './window-registry';
import type { WindowDisplay, WindowRegistry } from './window-registry';
import type { WindowInfo } from '../ipc/session-client';

const LAYOUT_MODULE = '../layout/float-layout';

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
 * Returns a stacking order no local computation would produce, so a registry that
 * renumbers zOrder itself cannot satisfy the delegation cases by accident.
 */
function stubbedStackingOrder(zOrders: Map<string, number>, windowId: string): Map<string, number> {
  const stacked = new Map<string, number>();
  [...zOrders.keys()].forEach((id, index) => {
    stacked.set(id, 1000 + index);
  });
  if (stacked.has(windowId)) {
    stacked.set(windowId, 9999);
  }
  return stacked;
}

async function importRegistryWithStubbedLayout(): Promise<{
  create: typeof createWindowRegistry;
  bringToFront: ReturnType<typeof vi.fn>;
}> {
  const bringToFront = vi.fn(stubbedStackingOrder);
  vi.resetModules();
  vi.doMock(LAYOUT_MODULE, () => ({ bringToFront }));
  const module = await import('./window-registry');
  return { create: module.createWindowRegistry, bringToFront };
}

afterEach(() => {
  vi.doUnmock(LAYOUT_MODULE);
  vi.resetModules();
});

describe('window registry layout state: initial display', () => {
  it('opens a window without a position so the layout can place it', () => {
    const registry = openRegistry('w-a');

    expect(displayOf(registry, 'w-a').position).toBeNull();
  });

  it('opens a window that is neither minimized nor maximized', () => {
    const registry = openRegistry('w-a');

    const display = displayOf(registry, 'w-a');
    expect({ isMinimized: display.isMinimized, isMaximized: display.isMaximized }).toEqual({
      isMinimized: false,
      isMaximized: false,
    });
  });

  it('opens a window without saved bounds to restore', () => {
    const registry = openRegistry('w-a');

    expect(displayOf(registry, 'w-a').restoreBounds).toBeNull();
  });
});

describe('window registry layout state: position and size', () => {
  it('updates the position of an open window', () => {
    const registry = openRegistry('w-a');

    registry.setWindowPosition('w-a', { x: 120, y: 64 });

    expect(displayOf(registry, 'w-a').position).toEqual({ x: 120, y: 64 });
  });

  it('updates the size of an open window', () => {
    const registry = openRegistry('w-a');

    registry.setWindowSize('w-a', { width: 720, height: 420 });

    expect(displayOf(registry, 'w-a').size).toEqual({ width: 720, height: 420 });
  });

  it('ignores a position update for an unknown window id', () => {
    const registry = openRegistry('w-a');

    expect(() => registry.setWindowPosition('w-missing', { x: 10, y: 20 })).not.toThrow();
    expect(get(registry).windows.map((entry) => entry.windowId)).toEqual(['w-a']);
  });

  it('ignores a size update for an unknown window id', () => {
    const registry = openRegistry('w-a');

    expect(() => registry.setWindowSize('w-missing', { width: 400, height: 300 })).not.toThrow();
    expect(get(registry).windows.map((entry) => entry.windowId)).toEqual(['w-a']);
  });
});

describe('window registry layout state: stacking order', () => {
  it('raises the target window above every other open window', () => {
    const registry = openRegistry('w-a', 'w-b', 'w-c');

    registry.bringWindowToFront('w-b');

    const zOrders = zOrdersOf(registry);
    expect(zOrders.get('w-b')).toBeGreaterThan(
      Math.max(zOrders.get('w-a') ?? 0, zOrders.get('w-c') ?? 0)
    );
  });

  it('keeps the stacking order unique across the open windows', () => {
    const registry = openRegistry('w-a', 'w-b', 'w-c');

    registry.bringWindowToFront('w-b');
    registry.bringWindowToFront('w-a');

    const zOrders = [...zOrdersOf(registry).values()];
    expect(new Set(zOrders).size).toBe(zOrders.length);
  });

  it('ignores a front request for an unknown window id', () => {
    const registry = openRegistry('w-a');

    expect(() => registry.bringWindowToFront('w-missing')).not.toThrow();
    expect(get(registry).windows.map((entry) => entry.windowId)).toEqual(['w-a']);
  });
});

describe('window registry layout state: stacking order is owned by float-layout', () => {
  it('hands the current stacking order of every open window to the layout module', async () => {
    const { create, bringToFront } = await importRegistryWithStubbedLayout();
    const registry = create();
    for (const windowId of ['w-a', 'w-b', 'w-c']) {
      registry.openWindow(makeWindowInfo(windowId));
    }
    const before = zOrdersOf(registry);

    registry.bringWindowToFront('w-b');

    expect(bringToFront).toHaveBeenCalledWith(before, 'w-b');
  });

  it('writes back the stacking order the layout module returned', async () => {
    const { create } = await importRegistryWithStubbedLayout();
    const registry = create();
    for (const windowId of ['w-a', 'w-b', 'w-c']) {
      registry.openWindow(makeWindowInfo(windowId));
    }

    registry.bringWindowToFront('w-b');

    expect(zOrdersOf(registry)).toEqual(
      new Map([
        ['w-a', 1000],
        ['w-b', 9999],
        ['w-c', 1002],
      ])
    );
  });
});

describe('window registry layout state: minimize', () => {
  it('records that a window is minimized', () => {
    const registry = openRegistry('w-a');

    registry.setWindowMinimized('w-a', true);

    expect(displayOf(registry, 'w-a').isMinimized).toBe(true);
  });

  it('clears the minimized state when it is turned off again', () => {
    const registry = openRegistry('w-a');

    registry.setWindowMinimized('w-a', true);
    registry.setWindowMinimized('w-a', false);

    expect(displayOf(registry, 'w-a').isMinimized).toBe(false);
  });

  it('ignores a minimize request for an unknown window id', () => {
    const registry = openRegistry('w-a');

    expect(() => registry.setWindowMinimized('w-missing', true)).not.toThrow();
    expect(displayOf(registry, 'w-a').isMinimized).toBe(false);
  });
});

describe('window registry layout state: maximize', () => {
  it('records that a window is maximized', () => {
    const registry = openRegistry('w-a');

    registry.setWindowMaximized('w-a', true);

    expect(displayOf(registry, 'w-a').isMaximized).toBe(true);
  });

  it('captures the rectangle the window had before it was maximized', () => {
    const registry = openRegistry('w-a');
    registry.setWindowPosition('w-a', { x: 40, y: 30 });
    registry.setWindowSize('w-a', { width: 720, height: 420 });

    registry.setWindowMaximized('w-a', true);

    expect(displayOf(registry, 'w-a').restoreBounds).toEqual({
      position: { x: 40, y: 30 },
      size: { width: 720, height: 420 },
    });
  });

  it('keeps the first captured rectangle when an already maximized window is maximized again', () => {
    const registry = openRegistry('w-a');
    registry.setWindowPosition('w-a', { x: 40, y: 30 });
    registry.setWindowSize('w-a', { width: 720, height: 420 });
    registry.setWindowMaximized('w-a', true);

    registry.setWindowPosition('w-a', { x: 0, y: 0 });
    registry.setWindowSize('w-a', { width: 1600, height: 900 });
    registry.setWindowMaximized('w-a', true);

    expect(displayOf(registry, 'w-a').restoreBounds).toEqual({
      position: { x: 40, y: 30 },
      size: { width: 720, height: 420 },
    });
  });

  it('puts the captured rectangle back when the window is restored', () => {
    const registry = openRegistry('w-a');
    registry.setWindowPosition('w-a', { x: 40, y: 30 });
    registry.setWindowSize('w-a', { width: 720, height: 420 });
    registry.setWindowMaximized('w-a', true);
    registry.setWindowPosition('w-a', { x: 0, y: 0 });
    registry.setWindowSize('w-a', { width: 1600, height: 900 });

    registry.setWindowMaximized('w-a', false);

    const display = displayOf(registry, 'w-a');
    expect({ position: display.position, size: display.size }).toEqual({
      position: { x: 40, y: 30 },
      size: { width: 720, height: 420 },
    });
  });

  it('drops the captured rectangle once the window is restored', () => {
    const registry = openRegistry('w-a');
    registry.setWindowPosition('w-a', { x: 40, y: 30 });
    registry.setWindowSize('w-a', { width: 720, height: 420 });
    registry.setWindowMaximized('w-a', true);

    registry.setWindowMaximized('w-a', false);

    expect(displayOf(registry, 'w-a').restoreBounds).toBeNull();
  });
});

describe('window registry layout state: at most one window is maximized', () => {
  it('restores the previously maximized window when another one is maximized', () => {
    const registry = openRegistry('w-a', 'w-b');
    registry.setWindowMaximized('w-a', true);

    registry.setWindowMaximized('w-b', true);

    expect({
      'w-a': displayOf(registry, 'w-a').isMaximized,
      'w-b': displayOf(registry, 'w-b').isMaximized,
    }).toEqual({ 'w-a': false, 'w-b': true });
  });

  it('restores the maximized window when a different window is brought to the front', () => {
    const registry = openRegistry('w-a', 'w-b');
    registry.setWindowMaximized('w-a', true);

    registry.bringWindowToFront('w-b');

    expect(displayOf(registry, 'w-a').isMaximized).toBe(false);
  });

  it('keeps a maximized window maximized when it is itself brought to the front', () => {
    const registry = openRegistry('w-a', 'w-b');
    registry.setWindowMaximized('w-a', true);

    registry.bringWindowToFront('w-a');

    expect(displayOf(registry, 'w-a').isMaximized).toBe(true);
  });
});

describe('window registry layout state: closing a window', () => {
  it('leaves no layout state behind for a window reopened under the same id', () => {
    const registry = openRegistry('w-a');
    registry.setWindowPosition('w-a', { x: 40, y: 30 });
    registry.setWindowSize('w-a', { width: 720, height: 420 });
    registry.setWindowMaximized('w-a', true);
    registry.setWindowMinimized('w-a', true);

    registry.closeWindow('w-a');
    registry.openWindow(makeWindowInfo('w-a'));

    const display = displayOf(registry, 'w-a');
    expect({
      position: display.position,
      size: display.size,
      isMinimized: display.isMinimized,
      isMaximized: display.isMaximized,
      restoreBounds: display.restoreBounds,
    }).toEqual({
      position: null,
      size: null,
      isMinimized: false,
      isMaximized: false,
      restoreBounds: null,
    });
  });
});
