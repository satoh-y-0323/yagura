/**
 * The registry is the single owner of which windows are open, which one is active,
 * and which events go to which window. These cases call it directly rather than
 * through Tauri event plumbing, so no-cross-wiring is checked without event timing.
 */
import { describe, it, expect, vi } from 'vitest';
import { get } from 'svelte/store';

import { createWindowRegistry } from './window-registry';
import type { WindowRegistry } from './window-registry';
import type { DetectedStatusView, UnknownReason } from '../ipc/session-boundary';
import type { WindowInfo, WindowEventHandlers } from '../ipc/session-client';

function makeWindowInfo(overrides: Partial<WindowInfo> = {}): WindowInfo {
  return {
    window_id: 'w1-k3f9xa',
    name: 'claude',
    vendor: 'claude-code',
    cwd: 'C:\\Users\\dev\\project',
    detected: { status: 'running' },
    exit_code: null,
    ...overrides,
  };
}

function makeHandlers(): WindowEventHandlers {
  return {
    onFrame: vi.fn(),
    onExit: vi.fn(),
    onDisconnected: vi.fn(),
    onSuperseded: vi.fn(),
  };
}

describe('createWindowRegistry: does not cross-wire windows', () => {
  it('does not deliver window A events to window B handlers', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));
    registry.openWindow(makeWindowInfo({ window_id: 'w-b' }));
    const handlersA = makeHandlers();
    const handlersB = makeHandlers();
    registry.registerWindowHandlers('w-a', handlersA);
    registry.registerWindowHandlers('w-b', handlersB);

    registry.resolveWindowHandlers('w-a')?.onExit({ window_id: 'w-a', exit_code: 0 });

    expect(handlersA.onExit).toHaveBeenCalledTimes(1);
    expect(handlersB.onExit).toHaveBeenCalledTimes(0);
  });
});

describe('createWindowRegistry: subscription model', () => {
  it('routes each window status change to that window only', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', detected: { status: 'running' } }));
    registry.openWindow(makeWindowInfo({ window_id: 'w-b', detected: { status: 'idle' } }));

    registry.setStatus('w-a', { status: 'waiting_input' }, null);

    const state = get(registry);
    const a = state.windows.find((w) => w.windowId === 'w-a');
    const b = state.windows.find((w) => w.windowId === 'w-b');
    expect(a?.detected).toEqual({ status: 'waiting_input' });
    expect(b?.detected).toEqual({ status: 'idle' });
  });
});

describe('createWindowRegistry: lifecycle', () => {
  it('discards the window entry once its status becomes exited', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));

    registry.setStatus('w-a', { status: 'exited' }, null);

    const state = get(registry);
    expect(state.windows.some((w) => w.windowId === 'w-a')).toBe(false);
  });

  it('clears to an empty state once the last window closes', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));

    registry.closeWindow('w-a');

    const state = get(registry);
    expect(state.windows).toEqual([]);
  });

  it('ignores a status change for a window id that is not in the list', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));

    expect(() => registry.setStatus('unknown-window', { status: 'running' }, null)).not.toThrow();
    expect(registry.resolveWindowHandlers('unknown-window')).toBeUndefined();
    expect(get(registry).windows).toHaveLength(1);
  });
});

describe('createWindowRegistry: renameWindow', () => {
  it('updates the name of the target window and leaves other windows unchanged', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', name: 'claude' }));
    registry.openWindow(makeWindowInfo({ window_id: 'w-b', name: 'codex' }));

    registry.renameWindow('w-a', 'renamed-a');

    const state = get(registry);
    const a = state.windows.find((w) => w.windowId === 'w-a');
    const b = state.windows.find((w) => w.windowId === 'w-b');
    expect(a?.name).toBe('renamed-a');
    expect(b?.name).toBe('codex');
  });

  it('changes only the name, not order or status', () => {
    const registry = createWindowRegistry();
    registry.openWindow(
      makeWindowInfo({ window_id: 'w-a', name: 'claude', detected: { status: 'running' } })
    );
    registry.openWindow(
      makeWindowInfo({ window_id: 'w-b', name: 'codex', detected: { status: 'idle' } })
    );
    const before = get(registry);

    registry.renameWindow('w-b', 'renamed-b');

    const after = get(registry);
    expect(after.windows.map((w) => w.windowId)).toEqual(before.windows.map((w) => w.windowId));
    const a = after.windows.find((w) => w.windowId === 'w-a');
    const b = after.windows.find((w) => w.windowId === 'w-b');
    expect(a?.detected.status).toBe('running');
    expect(b?.detected.status).toBe('idle');
  });

  it('is a no-op for a window id that is not open', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', name: 'claude' }));
    const before = get(registry);

    expect(() => registry.renameWindow('unknown-window', 'renamed')).not.toThrow();

    const after = get(registry);
    expect(after).toEqual(before);
  });
});

describe('createWindowRegistry: re-applying a window list snapshot', () => {
  it('picks up a new name for an already-open window without adding a duplicate entry', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', name: 'claude' }));

    // The periodic poll delivering a snapshot whose name changed elsewhere.
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', name: 'renamed-a' }));

    const state = get(registry);
    expect(state.windows).toHaveLength(1);
    const a = state.windows.find((w) => w.windowId === 'w-a');
    expect(a?.name).toBe('renamed-a');
  });
});

// ---------------------------------------------------------------------------
// The room a window belongs to travels with the window list snapshot: a new
// window copies it, and a snapshot of an already-open window refreshes it the
// way it refreshes the display name.
// ---------------------------------------------------------------------------

/** The entry a freshly opened window produces, before any display change. */
function openedEntry(
  windowId: string,
  name: string,
  room: { roomId: string; name: string } | null,
  zOrder = 0
) {
  return {
    windowId,
    name,
    vendor: 'claude-code',
    cwd: 'C:\\Users\\dev\\project',
    detected: { status: 'running' },
    statusRev: null,
    exitCode: null,
    room,
    detectionWarning: null,
    mcpConnected: null,
    statusReportsMissing: null,
    display: {
      position: null,
      size: null,
      zOrder,
      isMinimized: false,
      isMaximized: false,
      restoreBounds: null,
    },
  };
}

describe('createWindowRegistry: the room a window belongs to', () => {
  it('copies the room of a newly opened window', () => {
    const registry = createWindowRegistry();

    registry.openWindow(
      makeWindowInfo({ window_id: 'w-a', room: { room_id: 'r-alpha', name: 'yagura' } })
    );

    expect(get(registry).windows).toEqual([
      openedEntry('w-a', 'claude', { roomId: 'r-alpha', name: 'yagura' }),
    ]);
  });

  it('records a null room for a window whose snapshot carries none', () => {
    const registry = createWindowRegistry();

    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));

    expect(get(registry).windows).toEqual([openedEntry('w-a', 'claude', null)]);
  });

  it('picks up a new room for an already-open window, alongside the name', () => {
    const registry = createWindowRegistry();
    registry.openWindow(
      makeWindowInfo({ window_id: 'w-a', room: { room_id: 'r-alpha', name: 'yagura' } })
    );

    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-a',
        name: 'renamed-a',
        room: { room_id: 'r-beta', name: 'orca' },
      })
    );

    expect(get(registry).windows).toEqual([
      openedEntry('w-a', 'renamed-a', { roomId: 'r-beta', name: 'orca' }),
    ]);
  });

  it('clears the room of an already-open window when the snapshot carries none', () => {
    const registry = createWindowRegistry();
    registry.openWindow(
      makeWindowInfo({ window_id: 'w-a', room: { room_id: 'r-alpha', name: 'yagura' } })
    );

    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));

    expect(get(registry).windows).toEqual([openedEntry('w-a', 'claude', null)]);
  });
});

// ---------------------------------------------------------------------------
// Minimizing or restoring a set of windows is one update, so the view sees the
// whole set move at once rather than one window per notification.
// ---------------------------------------------------------------------------

function setWindowsMinimized(
  registry: WindowRegistry,
  windowIds: readonly string[],
  minimized: boolean
): void {
  registry.setWindowsMinimized(windowIds, minimized);
}

function openRegistry(...windowIds: string[]): WindowRegistry {
  const registry = createWindowRegistry();
  for (const windowId of windowIds) {
    registry.openWindow(makeWindowInfo({ window_id: windowId }));
  }
  return registry;
}

function minimizedFlags(registry: WindowRegistry): Record<string, boolean> {
  return Object.fromEntries(
    get(registry).windows.map((entry): [string, boolean] => [
      entry.windowId,
      entry.display.isMinimized,
    ])
  );
}

describe('createWindowRegistry: setWindowsMinimized', () => {
  it('minimizes every listed window and leaves the others alone', () => {
    const registry = openRegistry('w-a', 'w-b', 'w-c');

    setWindowsMinimized(registry, ['w-a', 'w-c'], true);

    expect(minimizedFlags(registry)).toEqual({ 'w-a': true, 'w-b': false, 'w-c': true });
  });

  it('writes the whole set in a single notification', () => {
    const registry = openRegistry('w-a', 'w-b', 'w-c');
    const listener = vi.fn();
    registry.subscribe(listener);
    listener.mockClear();

    setWindowsMinimized(registry, ['w-a', 'w-b', 'w-c'], true);

    expect(listener).toHaveBeenCalledTimes(1);
  });

  it('restores every listed window in a single notification', () => {
    const registry = openRegistry('w-a', 'w-b', 'w-c');
    setWindowsMinimized(registry, ['w-a', 'w-b', 'w-c'], true);
    const listener = vi.fn();
    registry.subscribe(listener);
    listener.mockClear();

    setWindowsMinimized(registry, ['w-a', 'w-b'], false);

    expect(listener).toHaveBeenCalledTimes(1);
    expect(minimizedFlags(registry)).toEqual({ 'w-a': false, 'w-b': false, 'w-c': true });
  });

  it('ignores a window id that is not open', () => {
    const registry = openRegistry('w-a', 'w-b');

    expect(() => {
      setWindowsMinimized(registry, ['w-a', 'w-missing'], true);
    }).not.toThrow();

    expect(minimizedFlags(registry)).toEqual({ 'w-a': true, 'w-b': false });
  });

  it('leaves the state unchanged for an empty list', () => {
    const registry = openRegistry('w-a', 'w-b');
    const before = get(registry);

    setWindowsMinimized(registry, [], true);

    expect(get(registry)).toEqual(before);
  });

  it('produces the same state as the single-window form', () => {
    const bulk = openRegistry('w-a', 'w-b');
    const single = openRegistry('w-a', 'w-b');

    setWindowsMinimized(bulk, ['w-b'], true);
    single.setWindowMinimized('w-b', true);

    expect(get(bulk)).toEqual(get(single));
  });
});

// ---------------------------------------------------------------------------
// The status and the reason a window is in are one value on the entry, and
// every observation of that value names the generation it was taken at. The
// window list is polled while status pushes arrive, so the two routes race:
// a list taken before a push must not put its older pair back. What the list
// alone can refresh -- the display name, the room, the detection warning --
// lands whatever the generation says, because discarding those would leave the
// view showing a name the daemon no longer uses.
// ---------------------------------------------------------------------------

/** The unknown arm of a status pair, with the reason it carries. */
function unknownPair(reason: UnknownReason | null): DetectedStatusView {
  return { status: 'unknown', reason };
}

/** The entry of `windowId`, or undefined when no such window is open. */
function entryOf(registry: WindowRegistry, windowId: string) {
  return get(registry).windows.find((w) => w.windowId === windowId);
}

describe('createWindowRegistry: the status and its reason are one value on the entry', () => {
  it('records a window opened without a reason as a pair with no reason arm', () => {
    const registry = createWindowRegistry();

    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));

    const entry = entryOf(registry, 'w-a');
    expect(entry?.detected).toEqual({ status: 'running' });
    expect(entry?.detectionWarning).toBeNull();
    expect(entry?.statusRev).toBeNull();
  });

  it('copies the pair and the detection warning of a newly opened window', () => {
    const registry = createWindowRegistry();

    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-a',
        detected: unknownPair('no_definition'),
        detection_warning: 'the profile did not resolve a definition',
      })
    );

    const entry = entryOf(registry, 'w-a');
    expect(entry?.detected).toEqual({ status: 'unknown', reason: 'no_definition' });
    expect(entry?.detectionWarning).toBe('the profile did not resolve a definition');
  });

  it('replaces the whole pair on a status change', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));

    registry.setStatus('w-a', unknownPair('detector_stopped'), null);

    expect(entryOf(registry, 'w-a')?.detected).toEqual({
      status: 'unknown',
      reason: 'detector_stopped',
    });
  });

  it('leaves no reason on the entry once the status is no longer unknown', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));
    registry.setStatus('w-a', unknownPair('detector_stopped'), null);

    registry.setStatus('w-a', { status: 'running' }, null);

    expect(entryOf(registry, 'w-a')?.detected).toEqual({ status: 'running' });
  });

  it('does not touch detectionWarning on a status change (only a window-list snapshot refreshes it)', () => {
    const registry = createWindowRegistry();
    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-a',
        detected: unknownPair('no_definition'),
        detection_warning: 'kept across status changes',
      })
    );

    registry.setStatus('w-a', unknownPair('screen_lost'), null);

    expect(entryOf(registry, 'w-a')?.detectionWarning).toBe('kept across status changes');
  });
});

describe('createWindowRegistry: only an observation at least as new moves the pair', () => {
  it('applies a status change taken at a newer generation', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 4 }));

    registry.setStatus('w-a', unknownPair('screen_lost'), 5);

    const entry = entryOf(registry, 'w-a');
    expect(entry?.detected).toEqual({ status: 'unknown', reason: 'screen_lost' });
    expect(entry?.statusRev).toBe(5);
  });

  it('leaves the pair where it is for a status change taken at an older generation', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 4 }));

    registry.setStatus('w-a', unknownPair('screen_lost'), 3);

    const entry = entryOf(registry, 'w-a');
    expect(entry?.detected).toEqual({ status: 'running' });
    expect(entry?.statusRev).toBe(4);
  });

  it('applies a status change taken at the same generation', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 4 }));

    registry.setStatus('w-a', { status: 'idle' }, 4);

    expect(entryOf(registry, 'w-a')?.detected).toEqual({ status: 'idle' });
  });

  it('applies a status change that names no generation at all', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 4 }));

    registry.setStatus('w-a', { status: 'idle' }, null);

    expect(entryOf(registry, 'w-a')?.detected).toEqual({ status: 'idle' });
  });

  it('keeps the recorded generation when it applies an observation that names none, so a later stale one is still refused', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 4 }));

    registry.setStatus('w-a', { status: 'idle' }, null);
    registry.setStatus('w-a', { status: 'interrupted' }, 3);

    const entry = entryOf(registry, 'w-a');
    expect(entry?.statusRev).toBe(4);
    expect(entry?.detected).toEqual({ status: 'idle' });
  });

  it('applies every observation of a window whose generation was never reported', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));

    registry.setStatus('w-a', { status: 'idle' }, null);
    expect(entryOf(registry, 'w-a')?.detected).toEqual({ status: 'idle' });

    registry.setStatus('w-a', { status: 'interrupted' }, 2);
    expect(entryOf(registry, 'w-a')?.detected).toEqual({ status: 'interrupted' });
  });

  it('compares generations per window, so a high generation on one window does not gate another', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 9 }));
    registry.openWindow(makeWindowInfo({ window_id: 'w-b', status_rev: 1 }));

    registry.setStatus('w-b', { status: 'idle' }, 2);

    expect(entryOf(registry, 'w-b')?.detected).toEqual({ status: 'idle' });
    expect(entryOf(registry, 'w-a')?.detected).toEqual({ status: 'running' });
  });
});

describe('createWindowRegistry: the window-list route compares generations too', () => {
  it('applies a snapshot taken at a newer generation', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 4 }));

    registry.openWindow(
      makeWindowInfo({ window_id: 'w-a', detected: unknownPair('screen_lost'), status_rev: 5 })
    );

    const entry = entryOf(registry, 'w-a');
    expect(entry?.detected).toEqual({ status: 'unknown', reason: 'screen_lost' });
    expect(entry?.statusRev).toBe(5);
  });

  it('leaves the pair where it is for a snapshot taken at an older generation', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));
    registry.setStatus('w-a', { status: 'waiting_input' }, 7);

    registry.openWindow(
      makeWindowInfo({ window_id: 'w-a', detected: unknownPair('screen_lost'), status_rev: 6 })
    );

    const entry = entryOf(registry, 'w-a');
    expect(entry?.detected).toEqual({ status: 'waiting_input' });
    expect(entry?.statusRev).toBe(7);
  });

  it('applies a snapshot taken at the same generation', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));
    registry.setStatus('w-a', { status: 'waiting_input' }, 7);

    registry.openWindow(
      makeWindowInfo({ window_id: 'w-a', detected: { status: 'idle' }, status_rev: 7 })
    );

    expect(entryOf(registry, 'w-a')?.detected).toEqual({ status: 'idle' });
  });

  it('applies a snapshot that names no generation at all', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));
    registry.setStatus('w-a', { status: 'waiting_input' }, 7);

    registry.openWindow(makeWindowInfo({ window_id: 'w-a', detected: { status: 'idle' } }));

    expect(entryOf(registry, 'w-a')?.detected).toEqual({ status: 'idle' });
  });

  it('records the generation a first snapshot of a window names', () => {
    const registry = createWindowRegistry();

    registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 12 }));

    expect(entryOf(registry, 'w-a')?.statusRev).toBe(12);
  });
});

describe('createWindowRegistry: an older generation costs the pair alone', () => {
  it('takes the display name, the room and the detection warning from a snapshot older than the pair it holds', () => {
    const registry = createWindowRegistry();
    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-a',
        name: 'claude',
        room: { room_id: 'r-alpha', name: 'yagura' },
        detection_warning: 'first warning',
      })
    );
    registry.setStatus('w-a', { status: 'waiting_input' }, 7);

    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-a',
        name: 'renamed',
        detected: unknownPair('screen_lost'),
        room: { room_id: 'r-beta', name: 'other' },
        detection_warning: 'second warning',
        status_rev: 2,
      })
    );

    const entry = entryOf(registry, 'w-a');
    expect(entry?.name).toBe('renamed');
    expect(entry?.room).toEqual({ roomId: 'r-beta', name: 'other' });
    expect(entry?.detectionWarning).toBe('second warning');
    expect(entry?.detected).toEqual({ status: 'waiting_input' });
  });

  it('clears the room and the detection warning a snapshot no longer carries, even when the snapshot is older', () => {
    const registry = createWindowRegistry();
    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-a',
        room: { room_id: 'r-alpha', name: 'yagura' },
        detection_warning: 'first warning',
      })
    );
    registry.setStatus('w-a', { status: 'waiting_input' }, 7);

    registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 1 }));

    const entry = entryOf(registry, 'w-a');
    expect(entry?.room).toBeNull();
    expect(entry?.detectionWarning).toBeNull();
    expect(entry?.detected).toEqual({ status: 'waiting_input' });
  });
});

describe('createWindowRegistry: only the status-change route closes a window', () => {
  it('keeps a window whose window-list snapshot reports it as exited', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));

    registry.openWindow(makeWindowInfo({ window_id: 'w-a', detected: { status: 'exited' } }));

    const entry = entryOf(registry, 'w-a');
    expect(entry).toBeDefined();
    expect(entry?.detected).toEqual({ status: 'exited' });
    expect(get(registry).tabOrder).toEqual(['w-a']);
  });

  it('opens a window whose very first snapshot already reports it as exited, leaving the windows already open in place', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));

    registry.openWindow(makeWindowInfo({ window_id: 'w-b', detected: { status: 'exited' } }));

    expect(entryOf(registry, 'w-a')).toBeDefined();
    expect(entryOf(registry, 'w-b')?.detected).toEqual({ status: 'exited' });
  });

  it('still removes a window when the status-change route reports it as exited', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));

    registry.setStatus('w-a', { status: 'exited' }, null);

    expect(entryOf(registry, 'w-a')).toBeUndefined();
  });
});

describe('createWindowRegistry: an entry never holds a reason without the unknown status', () => {
  /** True when the entry's pair keeps the reason on the unknown arm alone. */
  function pairHoldsTogether(detected: DetectedStatusView | undefined): boolean {
    if (detected === undefined) {
      return false;
    }
    return (
      detected.status === 'unknown' || !Object.prototype.hasOwnProperty.call(detected, 'reason')
    );
  }

  it('holds together when the status change arrives before the window-list snapshot', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', detected: unknownPair('screen_lost') }));

    registry.setStatus('w-a', { status: 'running' }, 2);
    expect(pairHoldsTogether(entryOf(registry, 'w-a')?.detected)).toBe(true);

    registry.openWindow(
      makeWindowInfo({ window_id: 'w-a', detected: unknownPair('no_definition'), status_rev: 3 })
    );
    expect(pairHoldsTogether(entryOf(registry, 'w-a')?.detected)).toBe(true);
  });

  it('holds together when the window-list snapshot arrives before the status change', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));

    registry.openWindow(
      makeWindowInfo({ window_id: 'w-a', detected: unknownPair('screen_lost'), status_rev: 2 })
    );
    expect(pairHoldsTogether(entryOf(registry, 'w-a')?.detected)).toBe(true);

    registry.setStatus('w-a', { status: 'idle' }, 3);
    const entry = entryOf(registry, 'w-a');
    expect(pairHoldsTogether(entry?.detected)).toBe(true);
    expect(entry?.detected).toEqual({ status: 'idle' });
  });

  it('holds together when a discarded older observation carried a reason', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));
    registry.setStatus('w-a', { status: 'running' }, 5);

    registry.setStatus('w-a', unknownPair('screen_lost'), 1);

    const entry = entryOf(registry, 'w-a');
    expect(pairHoldsTogether(entry?.detected)).toBe(true);
    expect(entry?.detected).toEqual({ status: 'running' });
  });
});
