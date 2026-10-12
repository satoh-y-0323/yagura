/**
 * The window the user is in is derived from what is on screen; neither the
 * registry nor a restored layout keeps a second record of it.
 */
import { describe, it, expect } from 'vitest';
import { get } from 'svelte/store';

import type { WindowInfo } from '../ipc/session-client';
import {
  applyLayoutSteps,
  planLayoutApplication,
  type LayoutTarget,
  type LayoutV1,
} from '../session/restore-model';
import { createWindowRegistry } from './window-registry';

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

function layout(overrides: Partial<LayoutV1>): LayoutV1 {
  return {
    v: 1,
    pos: { x: 10, y: 20 },
    size: { width: 300, height: 200 },
    z: 1,
    min: false,
    max: false,
    tab: 0,
    ...overrides,
  };
}

describe('window registry keeps no active window of its own', () => {
  it('holds no activeWindowId once a window is open', () => {
    const registry = createWindowRegistry();
    registry.openWindow(windowInfo('w-a'));

    const state = get(registry);
    expect(state.windows.map((entry) => entry.windowId)).toEqual(['w-a']);
    expect(state).not.toHaveProperty('activeWindowId');
  });

  it('holds no activeWindowId after the first of two windows closes, nor after the last one closes', () => {
    const registry = createWindowRegistry();
    registry.openWindow(windowInfo('w-a'));
    registry.openWindow(windowInfo('w-b'));

    registry.closeWindow('w-a');
    expect(get(registry).windows.map((entry) => entry.windowId)).toEqual(['w-b']);
    expect(get(registry)).not.toHaveProperty('activeWindowId');

    registry.closeWindow('w-b');
    expect(get(registry).windows).toEqual([]);
    expect(get(registry)).not.toHaveProperty('activeWindowId');
  });

  it('offers no setActiveWindow, while bringWindowToFront stays', () => {
    const registry = createWindowRegistry();

    expect(typeof registry.bringWindowToFront).toBe('function');
    expect(registry).not.toHaveProperty('setActiveWindow');
  });
});

describe('a restored layout plans no active-window step', () => {
  const plans = {
    'one plain window': planLayoutApplication(
      [{ windowId: 'w1', position: 0, layout: layout({}) }],
      ['w1']
    ),
    'one maximized window': planLayoutApplication(
      [{ windowId: 'w1', position: 0, layout: layout({ max: true }) }],
      ['w1']
    ),
    'a maximized window among plain ones': planLayoutApplication(
      [
        { windowId: 'w1', position: 0, layout: layout({ z: 2 }) },
        { windowId: 'w2', position: 1, layout: layout({ max: true, tab: 1 }) },
      ],
      ['w1', 'w2']
    ),
  };

  it.each(Object.entries(plans))(
    'still fronts or maximizes but plans no active step for %s',
    (_name, steps) => {
      const kinds: string[] = steps.map((step) => step.kind);

      expect(kinds.some((kind) => kind === 'front' || kind === 'maximize')).toBe(true);
      expect(kinds).not.toContain('active');
    }
  );

  it('applies a whole plan through a target that has no setActiveWindow', () => {
    const calls: string[] = [];
    const target: LayoutTarget = {
      setWindowPosition: (windowId) => calls.push(`position:${windowId}`),
      setWindowSize: (windowId) => calls.push(`size:${windowId}`),
      setWindowMinimized: (windowId) => calls.push(`minimized:${windowId}`),
      bringWindowToFront: (windowId) => calls.push(`front:${windowId}`),
      setWindowMaximized: (windowId) => calls.push(`maximize:${windowId}`),
      moveTab: (windowId) => calls.push(`tab:${windowId}`),
    };

    applyLayoutSteps(target, plans['one maximized window']);

    expect(calls).toEqual([
      'position:w1',
      'size:w1',
      'minimized:w1',
      'maximize:w1',
      'front:w1',
      'tab:w1',
    ]);
  });
});
