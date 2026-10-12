/**
 * The registry entry mirrors whether the window's child is connected to the
 * daemon over MCP, and follows every window-list refresh, including one that
 * stops saying anything about it.
 */
import { describe, it, expect } from 'vitest';
import { get } from 'svelte/store';

import { createWindowRegistry, type WindowRegistry } from './window-registry';
import type { WindowInfo } from '../ipc/session-client';

const WINDOW_ID = 'w-mcp';

function makeWindowInfo(overrides: Partial<WindowInfo> = {}): WindowInfo {
  return {
    window_id: WINDOW_ID,
    name: 'claude',
    vendor: 'claude-code',
    cwd: 'C:\\Users\\dev\\project',
    detected: { status: 'running' },
    exit_code: null,
    ...overrides,
  };
}

function mcpConnectedOf(registry: WindowRegistry): boolean | null | undefined {
  return get(registry).windows.find((entry) => entry.windowId === WINDOW_ID)?.mcpConnected;
}

describe('window registry: mcpConnected', () => {
  it('records true from the snapshot that opens the window', () => {
    const registry = createWindowRegistry();

    registry.openWindow(makeWindowInfo({ mcp_connected: true }));

    expect(mcpConnectedOf(registry)).toBe(true);
  });

  it('records false from the snapshot that opens the window', () => {
    const registry = createWindowRegistry();

    registry.openWindow(makeWindowInfo({ mcp_connected: false }));

    expect(mcpConnectedOf(registry)).toBe(false);
  });

  it('records null when the snapshot that opens the window says nothing', () => {
    const registry = createWindowRegistry();

    registry.openWindow(makeWindowInfo());

    expect(mcpConnectedOf(registry)).toBeNull();
  });

  it('follows list refreshes from true to false to null', () => {
    const registry = createWindowRegistry();

    registry.openWindow(makeWindowInfo({ mcp_connected: true }));
    expect(mcpConnectedOf(registry)).toBe(true);

    registry.openWindow(makeWindowInfo({ mcp_connected: false }));
    expect(mcpConnectedOf(registry)).toBe(false);

    registry.openWindow(makeWindowInfo());
    expect(mcpConnectedOf(registry)).toBeNull();
  });
});
