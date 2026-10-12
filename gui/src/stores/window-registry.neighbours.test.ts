/**
 * What one registry operation must leave alone: other windows, unknown ids, and the daemon's
 * handler routing once a window is gone.
 */
import { describe, it, expect, vi } from 'vitest';
import { get } from 'svelte/store';

import { createWindowRegistry } from './window-registry';
import type { WindowEntry, WindowRegistry } from './window-registry';
import type { WindowEventHandlers, WindowInfo } from '../ipc/session-client';

function info(windowId: string, overrides: Partial<WindowInfo> = {}): WindowInfo {
  return {
    window_id: windowId,
    name: `name-${windowId}`,
    vendor: null,
    cwd: '/work',
    detected: { status: 'running' },
    exit_code: null,
    ...overrides,
  };
}

function openRegistry(...windowIds: string[]): WindowRegistry {
  const registry = createWindowRegistry();
  for (const windowId of windowIds) registry.openWindow(info(windowId));
  return registry;
}

function entryOf(registry: WindowRegistry, windowId: string): WindowEntry {
  const entry = get(registry).windows.find((candidate) => candidate.windowId === windowId);
  if (entry === undefined) throw new Error(`window ${windowId} is not open`);
  return entry;
}

const HANDLERS: WindowEventHandlers = {
  onFrame: vi.fn(() => null),
  onExit: vi.fn(),
  onDisconnected: vi.fn(),
  onSuperseded: vi.fn(),
};

describe('window handlers follow the window', () => {
  it('forgets the handlers of a closed window', () => {
    const registry = openRegistry('w1');
    registry.registerWindowHandlers('w1', HANDLERS);

    registry.closeWindow('w1');

    expect(registry.resolveWindowHandlers('w1')).toBeUndefined();
  });

  it('forgets the handlers of a window whose child exited', () => {
    const registry = openRegistry('w1');
    registry.registerWindowHandlers('w1', HANDLERS);

    registry.setStatus('w1', { status: 'exited' }, null);

    expect(registry.resolveWindowHandlers('w1')).toBeUndefined();
  });

  it('keeps the handlers of the other windows', () => {
    const registry = openRegistry('w1', 'w2');
    registry.registerWindowHandlers('w1', HANDLERS);
    registry.registerWindowHandlers('w2', HANDLERS);

    registry.closeWindow('w1');

    expect(registry.resolveWindowHandlers('w2')).toBe(HANDLERS);
  });
});

describe('maximizing leaves other windows alone unless it takes the workspace', () => {
  it('keeps another window maximized when a window is un-maximized', () => {
    const registry = openRegistry('w1', 'w2');
    registry.setWindowMaximized('w1', true);
    registry.setWindowMaximized('w2', true);
    expect(entryOf(registry, 'w1').display.isMaximized).toBe(false);

    registry.setWindowMaximized('w1', false);

    expect(entryOf(registry, 'w2').display.isMaximized).toBe(true);
  });

  it('keeps a maximized window maximized when an unknown window is maximized', () => {
    const registry = openRegistry('w1');
    registry.setWindowMaximized('w1', true);

    registry.setWindowMaximized('missing', true);

    expect(entryOf(registry, 'w1').display.isMaximized).toBe(true);
  });

  it('keeps a maximized window maximized and the stacking as it is when an unknown window is raised', () => {
    const registry = openRegistry('w1', 'w2', 'w3');
    registry.bringWindowToFront('w1');
    registry.setWindowMaximized('w1', true);
    const before = get(registry).windows.map((entry) => entry.display);

    registry.bringWindowToFront('missing');

    expect(get(registry).windows.map((entry) => entry.display)).toEqual(before);
  });

  it('remembers no bounds to restore when the window had only a position', () => {
    const registry = openRegistry('w1');
    registry.setWindowPosition('w1', { x: 5, y: 6 });

    registry.setWindowMaximized('w1', true);

    expect(entryOf(registry, 'w1').display.restoreBounds).toBeNull();
  });

  it('remembers no bounds to restore when the window had only a size', () => {
    const registry = openRegistry('w1');
    registry.setWindowSize('w1', { width: 500, height: 300 });

    registry.setWindowMaximized('w1', true);

    expect(entryOf(registry, 'w1').display.restoreBounds).toBeNull();
  });

  it('keeps the position and size of a window that was never maximized when it is un-maximized', () => {
    const registry = openRegistry('w1');
    registry.setWindowPosition('w1', { x: 5, y: 6 });
    registry.setWindowSize('w1', { width: 500, height: 300 });

    registry.setWindowMaximized('w1', false);

    expect(entryOf(registry, 'w1').display).toMatchObject({
      position: { x: 5, y: 6 },
      size: { width: 500, height: 300 },
      isMaximized: false,
      restoreBounds: null,
    });
  });
});

describe('a snapshot of an open window refreshes that window only', () => {
  it('leaves the name, room and detection warning of the other windows alone', () => {
    const registry = openRegistry('w1', 'w2');

    registry.openWindow(
      info('w1', {
        name: 'renamed',
        room: { room_id: 'r1', name: 'room' },
        detection_warning: 'bad definition',
        mcp_connected: true,
        status_reports_missing: true,
      })
    );

    expect(entryOf(registry, 'w2')).toMatchObject({
      name: 'name-w2',
      room: null,
      detectionWarning: null,
      mcpConnected: null,
      statusReportsMissing: null,
    });
    expect(entryOf(registry, 'w1')).toMatchObject({
      name: 'renamed',
      room: { roomId: 'r1', name: 'room' },
      detectionWarning: 'bad definition',
      mcpConnected: true,
      statusReportsMissing: true,
    });
  });

  it('leaves the status of the other windows alone', () => {
    const registry = openRegistry('w1', 'w2');

    registry.openWindow(info('w1', { detected: { status: 'idle' } }));

    expect(entryOf(registry, 'w2').detected).toEqual({ status: 'running' });
  });
});

describe('status generations', () => {
  it('applies a numbered observation to a window that has recorded no generation, whatever the number', () => {
    const registry = openRegistry('w1');

    registry.setStatus('w1', { status: 'idle' }, -1);

    expect(entryOf(registry, 'w1')).toMatchObject({ detected: { status: 'idle' }, statusRev: -1 });
  });

  it('refuses an observation older than the recorded generation and accepts an equal one', () => {
    const registry = openRegistry('w1');
    registry.setStatus('w1', { status: 'idle' }, 5);

    registry.setStatus('w1', { status: 'running' }, 4);
    expect(entryOf(registry, 'w1').detected).toEqual({ status: 'idle' });

    registry.setStatus('w1', { status: 'running' }, 5);
    expect(entryOf(registry, 'w1').detected).toEqual({ status: 'running' });
  });

  it('leaves the other windows alone when one window is observed', () => {
    const registry = openRegistry('w1', 'w2');

    registry.setStatus('w1', { status: 'idle' }, 1);

    expect(entryOf(registry, 'w2').detected).toEqual({ status: 'running' });
  });
});

describe('closing a window', () => {
  it('leaves the other windows and their tabs as they were', () => {
    const registry = openRegistry('w1', 'w2', 'w3');

    registry.closeWindow('w2');

    expect(get(registry).windows.map((entry) => entry.windowId)).toEqual(['w1', 'w3']);
    expect(get(registry).tabOrder).toEqual(['w1', 'w3']);
  });

  it('leaves everything as it was when the id is unknown', () => {
    const registry = openRegistry('w1', 'w2');
    const before = structuredClone(get(registry));

    registry.closeWindow('missing');

    expect(get(registry)).toEqual(before);
  });
});

describe('operations on one window leave the others alone', () => {
  it('renames only the named window', () => {
    const registry = openRegistry('w1', 'w2');

    registry.renameWindow('w1', 'new');

    expect([entryOf(registry, 'w1').name, entryOf(registry, 'w2').name]).toEqual([
      'new',
      'name-w2',
    ]);
  });

  it('leaves everything as it was when the renamed id is unknown', () => {
    const registry = openRegistry('w1');
    const before = structuredClone(get(registry));

    registry.renameWindow('missing', 'new');

    expect(get(registry)).toEqual(before);
  });

  it('minimizes only the listed windows', () => {
    const registry = openRegistry('w1', 'w2', 'w3');

    registry.setWindowsMinimized(['w1', 'w3', 'missing'], true);

    expect(get(registry).windows.map((entry) => entry.display.isMinimized)).toEqual([
      true,
      false,
      true,
    ]);
  });

  it('moves only the named window', () => {
    const registry = openRegistry('w1', 'w2');

    registry.setWindowPosition('w2', { x: 3, y: 4 });

    expect(entryOf(registry, 'w1').display.position).toBeNull();
    expect(entryOf(registry, 'w2').display.position).toEqual({ x: 3, y: 4 });
  });
});
