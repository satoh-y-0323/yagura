/**
 * `tabOrder` is first-class registry state rather than a store of its own, so a
 * window and its tab appear or disappear in the same update. The multiset
 * invariant below is asserted per lifecycle path rather than once, so a failure
 * names the path that broke it.
 */
import { describe, it, expect } from 'vitest';
import { get } from 'svelte/store';

import { createWindowRegistry, type WindowRegistry } from './window-registry';
import type { WindowInfo } from '../ipc/session-client';

function makeWindowInfo(windowId: string, overrides: Partial<WindowInfo> = {}): WindowInfo {
  return {
    window_id: windowId,
    name: windowId,
    vendor: null,
    cwd: 'C:\\Users\\dev\\project',
    detected: { status: 'running' },
    exit_code: null,
    ...overrides,
  };
}

function openRegistry(...windowIds: string[]): WindowRegistry {
  const registry = createWindowRegistry();
  for (const windowId of windowIds) {
    registry.openWindow(makeWindowInfo(windowId));
  }
  return registry;
}

function tabOrderOf(registry: WindowRegistry): string[] {
  return get(registry).tabOrder;
}

function windowIdsOf(registry: WindowRegistry): string[] {
  return get(registry).windows.map((entry) => entry.windowId);
}

/** The multiset invariant: the same ids as `windows`, no duplicates. */
function expectTabOrderCoversWindows(registry: WindowRegistry): void {
  const tabOrder = tabOrderOf(registry);
  expect([...tabOrder].sort()).toEqual([...windowIdsOf(registry)].sort());
  expect(new Set(tabOrder).size).toBe(tabOrder.length);
}

/** Replays a window-list snapshot the way the periodic poll does. */
function applySnapshot(registry: WindowRegistry, ...windowIds: string[]): void {
  for (const windowId of windowIds) {
    registry.openWindow(makeWindowInfo(windowId));
  }
}

describe('window registry tab order: opening a window', () => {
  it('starts with an empty tab order', () => {
    expect(tabOrderOf(createWindowRegistry())).toEqual([]);
  });

  it('appends a newly opened window to the end of the tab order', () => {
    const registry = openRegistry('w-a');

    registry.openWindow(makeWindowInfo('w-b'));

    expect(tabOrderOf(registry)).toEqual(['w-a', 'w-b']);
  });

  it('keeps the tab order in the order the windows were opened', () => {
    expect(tabOrderOf(openRegistry('w-a', 'w-b', 'w-c'))).toEqual(['w-a', 'w-b', 'w-c']);
  });
});

describe('window registry tab order: re-applying a window list snapshot', () => {
  it('does not grow the tab order when the same snapshot is applied again', () => {
    const registry = openRegistry('w-a', 'w-b');

    applySnapshot(registry, 'w-a', 'w-b');

    expect(tabOrderOf(registry)).toEqual(['w-a', 'w-b']);
  });

  it('does not grow the tab order over repeated polls', () => {
    const registry = openRegistry('w-a', 'w-b');

    for (let poll = 0; poll < 5; poll++) {
      applySnapshot(registry, 'w-a', 'w-b');
    }

    expect(tabOrderOf(registry)).toEqual(['w-a', 'w-b']);
  });

  it('does not append when a refresh snapshot carries a new name', () => {
    const registry = openRegistry('w-a');

    registry.openWindow(makeWindowInfo('w-a', { name: 'renamed-a' }));

    expect(tabOrderOf(registry)).toEqual(['w-a']);
  });

  it('keeps a tab position a drag produced when the poll re-applies the snapshot', () => {
    const registry = openRegistry('w-a', 'w-b', 'w-c');
    registry.moveTab('w-c', 0);

    applySnapshot(registry, 'w-a', 'w-b', 'w-c');

    expect(tabOrderOf(registry)).toEqual(['w-c', 'w-a', 'w-b']);
  });
});

describe('window registry tab order: removing a window', () => {
  it('drops a closed window from the tab order', () => {
    const registry = openRegistry('w-a', 'w-b', 'w-c');

    registry.closeWindow('w-b');

    expect(tabOrderOf(registry)).toEqual(['w-a', 'w-c']);
  });

  it('drops a window from the tab order once its status becomes exited', () => {
    const registry = openRegistry('w-a', 'w-b', 'w-c');

    registry.setStatus('w-b', { status: 'exited' }, null);

    expect(tabOrderOf(registry)).toEqual(['w-a', 'w-c']);
  });

  it('empties the tab order once the last window is gone', () => {
    const registry = openRegistry('w-a');

    registry.closeWindow('w-a');

    expect(tabOrderOf(registry)).toEqual([]);
  });

  it('leaves the tab order alone when an unknown window id is closed', () => {
    const registry = openRegistry('w-a', 'w-b');

    registry.closeWindow('w-missing');

    expect(tabOrderOf(registry)).toEqual(['w-a', 'w-b']);
  });

  it('keeps the tab position of a window that was moved before another one closed', () => {
    const registry = openRegistry('w-a', 'w-b', 'w-c');
    registry.moveTab('w-c', 0);

    registry.closeWindow('w-a');

    expect(tabOrderOf(registry)).toEqual(['w-c', 'w-b']);
  });
});

describe('window registry tab order: moveTab', () => {
  it('moves a tab to the front', () => {
    const registry = openRegistry('w-a', 'w-b', 'w-c');

    registry.moveTab('w-c', 0);

    expect(tabOrderOf(registry)).toEqual(['w-c', 'w-a', 'w-b']);
  });

  it('moves a tab to the end', () => {
    const registry = openRegistry('w-a', 'w-b', 'w-c');

    registry.moveTab('w-a', 2);

    expect(tabOrderOf(registry)).toEqual(['w-b', 'w-c', 'w-a']);
  });

  it('leaves the tab order unchanged for an unknown window id', () => {
    const registry = openRegistry('w-a', 'w-b');

    expect(() => registry.moveTab('w-missing', 0)).not.toThrow();
    expect(tabOrderOf(registry)).toEqual(['w-a', 'w-b']);
  });

  it('does not reorder the window list itself (the open order stays visible elsewhere)', () => {
    const registry = openRegistry('w-a', 'w-b', 'w-c');

    registry.moveTab('w-c', 0);

    expect(windowIdsOf(registry)).toEqual(['w-a', 'w-b', 'w-c']);
  });
});

describe('window registry tab order: the multiset invariant holds on every path', () => {
  it('holds after a new window is opened', () => {
    const registry = openRegistry('w-a', 'w-b');

    expectTabOrderCoversWindows(registry);
  });

  it('holds after a refresh snapshot of an already-open window', () => {
    const registry = openRegistry('w-a', 'w-b');

    applySnapshot(registry, 'w-a', 'w-b');

    expectTabOrderCoversWindows(registry);
  });

  it('holds after a rename', () => {
    const registry = openRegistry('w-a', 'w-b');

    registry.renameWindow('w-a', 'renamed-a');

    expectTabOrderCoversWindows(registry);
  });

  it('holds after a rename of a window id that is not open', () => {
    const registry = openRegistry('w-a', 'w-b');

    registry.renameWindow('w-missing', 'renamed');

    expectTabOrderCoversWindows(registry);
  });

  it('holds after a close', () => {
    const registry = openRegistry('w-a', 'w-b', 'w-c');

    registry.closeWindow('w-b');

    expectTabOrderCoversWindows(registry);
  });

  it('holds after a window exits', () => {
    const registry = openRegistry('w-a', 'w-b', 'w-c');

    registry.setStatus('w-b', { status: 'exited' }, null);

    expectTabOrderCoversWindows(registry);
  });

  it('holds after repeated polls of the same snapshot', () => {
    const registry = openRegistry('w-a', 'w-b');

    for (let poll = 0; poll < 3; poll++) {
      applySnapshot(registry, 'w-a', 'w-b');
    }

    expectTabOrderCoversWindows(registry);
  });

  it('holds after a tab is moved', () => {
    const registry = openRegistry('w-a', 'w-b', 'w-c');

    registry.moveTab('w-c', 0);

    expectTabOrderCoversWindows(registry);
  });

  it('holds after a window is reopened under an id that was closed', () => {
    const registry = openRegistry('w-a', 'w-b');
    registry.closeWindow('w-a');

    registry.openWindow(makeWindowInfo('w-a'));

    expectTabOrderCoversWindows(registry);
    expect(tabOrderOf(registry)).toEqual(['w-b', 'w-a']);
  });
});

describe('window registry tab order: single-maximize does not regress', () => {
  it('still restores the previously maximized window when another one is maximized', () => {
    const registry = openRegistry('w-a', 'w-b');
    registry.setWindowMaximized('w-a', true);

    registry.setWindowMaximized('w-b', true);

    const maximized = get(registry).windows.filter((entry) => entry.display.isMaximized);
    expect(maximized.map((entry) => entry.windowId)).toEqual(['w-b']);
  });

  it('still restores the maximized window when another one is brought to the front', () => {
    const registry = openRegistry('w-a', 'w-b');
    registry.setWindowMaximized('w-a', true);

    registry.bringWindowToFront('w-b');

    const maximized = get(registry).windows.filter((entry) => entry.display.isMaximized);
    expect(maximized).toEqual([]);
  });

  it('leaves the tab order untouched when the maximized window changes', () => {
    const registry = openRegistry('w-a', 'w-b', 'w-c');
    registry.moveTab('w-c', 0);

    registry.setWindowMaximized('w-a', true);
    registry.setWindowMaximized('w-b', true);

    expect(tabOrderOf(registry)).toEqual(['w-c', 'w-a', 'w-b']);
  });

  it('leaves the tab order untouched when a window is minimized', () => {
    const registry = openRegistry('w-a', 'w-b');

    registry.setWindowMinimized('w-a', true);

    expect(tabOrderOf(registry)).toEqual(['w-a', 'w-b']);
  });
});
