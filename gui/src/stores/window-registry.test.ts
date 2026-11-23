/**
 * The registry is the single owner of which windows are open, which one is active,
 * and which events go to which window. These cases call it directly rather than
 * through Tauri event plumbing, so no-cross-wiring is checked without event timing.
 */
import { describe, it, expect, vi } from 'vitest';
import { get } from 'svelte/store';

import {
  applyListedWindow,
  applyStatusPush,
  createWindowRegistry,
  retirementFromList,
  retirementFromStatus,
} from './window-registry';
import { MAX_DISPLAY_FIELD_CHARS } from '../display/untrusted-text';
import { resolveMessages } from '../i18n/messages';
import type { WindowRegistry, WindowRegistryState } from './window-registry';
import type { DetectedStatusView, StatusPayload, UnknownReason } from '../ipc/session-boundary';
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
    onLink: vi.fn(),
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

    registry.setStatus('w-a', { status: 'waiting_input' }, null, null);

    const state = get(registry);
    const a = state.windows.find((w) => w.windowId === 'w-a');
    const b = state.windows.find((w) => w.windowId === 'w-b');
    expect(a?.detected).toEqual({ status: 'waiting_input' });
    expect(b?.detected).toEqual({ status: 'idle' });
  });
});

/** Every state the registry publishes from now on, leaving out the one subscribing replays. */
function publishedStates(registry: WindowRegistry): WindowRegistryState[] {
  const states: WindowRegistryState[] = [];
  let replayed = false;
  registry.subscribe((state) => {
    if (replayed) {
      states.push(state);
    }
    replayed = true;
  });
  return states;
}

describe('createWindowRegistry: lifecycle', () => {
  it('retires a window in one update that names the window and the code it ended with', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));
    registry.openWindow(makeWindowInfo({ window_id: 'w-b' }));
    registry.registerWindowHandlers('w-a', makeHandlers());
    const states = publishedStates(registry);

    registry.retireWindow('w-a', 3);
    registry.retireWindow('w-b', null);

    // A subscriber reads the code from the update that removes the window, not from an earlier one.
    expect(states).toHaveLength(2);
    expect(states[0]?.windows.map((entry) => entry.windowId)).toEqual(['w-b']);
    expect(states[0]?.tabOrder).toEqual(['w-b']);
    expect(states[0]?.retired).toEqual([{ windowId: 'w-a', exitCode: 3 }]);
    expect(states[1]?.windows).toEqual([]);
    expect(states[1]?.retired).toEqual([{ windowId: 'w-b', exitCode: null }]);
    expect(registry.resolveWindowHandlers('w-a')).toBeUndefined();
  });

  it('does nothing when retiring a window that is no longer listed', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));
    registry.openWindow(makeWindowInfo({ window_id: 'w-b' }));
    registry.retireWindow('w-a', 3);
    const states = publishedStates(registry);

    registry.retireWindow('w-a', 4);

    expect(get(registry).windows.map((entry) => entry.windowId)).toEqual(['w-b']);
    expect(states.flatMap((state) => state.retired)).toEqual([]);
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

    expect(() =>
      registry.setStatus('unknown-window', { status: 'running' }, null, null)
    ).not.toThrow();
    expect(registry.resolveWindowHandlers('unknown-window')).toBeUndefined();
    expect(get(registry).windows).toHaveLength(1);
  });
});

describe('createWindowRegistry: renameWindow', () => {
  it('updates the name of the target window and leaves other windows unchanged', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', name: 'claude' }));
    registry.openWindow(makeWindowInfo({ window_id: 'w-b', name: 'codex' }));

    registry.renameWindow('w-a', 'renamed\u202E-a\u0007');

    const state = get(registry);
    const a = state.windows.find((w) => w.windowId === 'w-a');
    const b = state.windows.find((w) => w.windowId === 'w-b');
    expect(a?.name).toBe('renamed\u202E-a\u0007');
    expect(a?.displayName).toBe('renamed\\u{202E}-a ');
    expect(b?.name).toBe('codex');
    expect(b?.displayName).toBe('codex');
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
    displayName: name,
    vendor: 'claude-code',
    displayVendor: 'claude-code',
    cwd: 'C:\\Users\\dev\\project',
    displayCwd: 'C:\\Users\\dev\\project',
    detected: { status: 'running' },
    statusRev: null,
    statusBoot: null,
    room: room === null ? null : { roomId: room.roomId, displayName: room.name },
    displayDetectionWarning: null,
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
        room: { room_id: 'r-beta', name: 'harbor' },
      })
    );

    expect(get(registry).windows).toEqual([
      openedEntry('w-a', 'renamed-a', { roomId: 'r-beta', name: 'harbor' }),
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

describe('createWindowRegistry: what the view draws of a newly opened window', () => {
  it('keeps the daemon values raw, and writes out what hides in a label and in a place', () => {
    const registry = createWindowRegistry();
    const rawCwd = `C:\\work\\\u202E${'a'.repeat(MAX_DISPLAY_FIELD_CHARS)}`;

    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-a',
        name: 'cl\u202Eaude\u0007',
        vendor: 'claude\u2066-code\u0085',
        cwd: rawCwd,
        room: { room_id: 'r-alpha', name: 'ya\u200Bgura\uFEFF' },
        detection_warning: 'the profile\u2028 did not\u001b resolve',
      })
    );
    registry.openWindow(makeWindowInfo({ window_id: 'w-b', vendor: null }));

    const a = entryOf(registry, 'w-a');
    expect(a?.name).toBe('cl\u202Eaude\u0007');
    expect(a?.displayName).toBe('cl\\u{202E}aude ');
    expect(a?.vendor).toBe('claude\u2066-code\u0085');
    expect(a?.displayVendor).toBe('claude\\u{2066}-code ');
    expect(a?.cwd).toBe(rawCwd);
    expect(a?.displayCwd).toBe(`C:\\work\\\\u{202E}${'a'.repeat(MAX_DISPLAY_FIELD_CHARS - 9)}…`);
    expect(a?.room).toEqual({
      roomId: 'r-alpha',
      displayName: 'ya\\u{200B}gura\\u{FEFF}',
    });
    expect(a?.displayDetectionWarning).toBe('the profile  did not  resolve');
    expect(entryOf(registry, 'w-b')?.displayVendor).toBeNull();
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
    expect(entry?.displayDetectionWarning).toBeNull();
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
    expect(entry?.displayDetectionWarning).toBe('the profile did not resolve a definition');
  });

  it('replaces the whole pair on a status change', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));

    registry.setStatus('w-a', unknownPair('detector_stopped'), null, null);

    expect(entryOf(registry, 'w-a')?.detected).toEqual({
      status: 'unknown',
      reason: 'detector_stopped',
    });
  });

  it('leaves no reason on the entry once the status is no longer unknown', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));
    registry.setStatus('w-a', unknownPair('detector_stopped'), null, null);

    registry.setStatus('w-a', { status: 'running' }, null, null);

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

    registry.setStatus('w-a', unknownPair('screen_lost'), null, null);

    expect(entryOf(registry, 'w-a')?.displayDetectionWarning).toBe('kept across status changes');
  });
});

describe('createWindowRegistry: only an observation at least as new moves the pair', () => {
  it('applies a status change taken at a newer generation', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 4 }));

    registry.setStatus('w-a', unknownPair('screen_lost'), 5, null);

    const entry = entryOf(registry, 'w-a');
    expect(entry?.detected).toEqual({ status: 'unknown', reason: 'screen_lost' });
    expect(entry?.statusRev).toBe(5);

    // A later boot of the same series is newer whatever its generation.
    registry.openWindow(
      makeWindowInfo({ window_id: 'w-b', status_rev: 4, status_boot: { origin: 7, seq: 1 } })
    );
    registry.setStatus('w-b', unknownPair('screen_lost'), 1, { origin: 7, seq: 2 });

    const later = entryOf(registry, 'w-b');
    expect(later?.detected).toEqual({ status: 'unknown', reason: 'screen_lost' });
    expect(later?.statusRev).toBe(1);
    expect(later?.statusBoot).toEqual({ origin: 7, seq: 2 });
  });

  it('leaves the pair where it is for a status change taken at an older generation', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 4 }));

    registry.setStatus('w-a', unknownPair('screen_lost'), 3, null);

    const entry = entryOf(registry, 'w-a');
    expect(entry?.detected).toEqual({ status: 'running' });
    expect(entry?.statusRev).toBe(4);

    // An earlier boot of the same series is older whatever its generation.
    registry.openWindow(
      makeWindowInfo({ window_id: 'w-b', status_rev: 1, status_boot: { origin: 7, seq: 2 } })
    );
    registry.setStatus('w-b', unknownPair('screen_lost'), 9, { origin: 7, seq: 1 });

    const earlier = entryOf(registry, 'w-b');
    expect(earlier?.detected).toEqual({ status: 'running' });
    expect(earlier?.statusRev).toBe(1);
    expect(earlier?.statusBoot).toEqual({ origin: 7, seq: 2 });

    // Within one boot, an older generation is older.
    registry.openWindow(
      makeWindowInfo({ window_id: 'w-c', status_rev: 5, status_boot: { origin: 7, seq: 2 } })
    );
    registry.setStatus('w-c', unknownPair('screen_lost'), 3, { origin: 7, seq: 2 });

    const sameBoot = entryOf(registry, 'w-c');
    expect(sameBoot?.detected).toEqual({ status: 'running' });
    expect(sameBoot?.statusRev).toBe(5);
  });

  it('applies a status change taken at the same generation', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 4 }));

    registry.setStatus('w-a', { status: 'idle' }, 4, null);

    expect(entryOf(registry, 'w-a')?.detected).toEqual({ status: 'idle' });

    // The same generation within the same boot is applied too.
    registry.openWindow(
      makeWindowInfo({ window_id: 'w-b', status_rev: 5, status_boot: { origin: 7, seq: 2 } })
    );
    registry.setStatus('w-b', { status: 'idle' }, 5, { origin: 7, seq: 2 });

    expect(entryOf(registry, 'w-b')?.detected).toEqual({ status: 'idle' });
  });

  it('applies a status change that names no generation at all', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 4 }));

    registry.setStatus('w-a', { status: 'idle' }, null, null);

    expect(entryOf(registry, 'w-a')?.detected).toEqual({ status: 'idle' });

    // Against a window that names a boot, an observation naming none cannot be ordered.
    registry.openWindow(
      makeWindowInfo({ window_id: 'w-b', status_rev: 5, status_boot: { origin: 7, seq: 2 } })
    );
    registry.setStatus('w-b', { status: 'idle' }, null, null);

    const unordered = entryOf(registry, 'w-b');
    expect(unordered?.detected).toEqual({ status: 'idle' });
    expect(unordered?.statusRev).toBeNull();
    expect(unordered?.statusBoot).toBeNull();
  });

  it('keeps the recorded generation when it applies an observation that names none, so a later stale one is still refused', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 4 }));

    registry.setStatus('w-a', { status: 'idle' }, null, null);
    registry.setStatus('w-a', { status: 'interrupted' }, 3, null);

    const entry = entryOf(registry, 'w-a');
    expect(entry?.statusRev).toBe(4);
    expect(entry?.detected).toEqual({ status: 'idle' });

    // Within one boot the generation is kept the same way; a later boot does not inherit it.
    registry.openWindow(
      makeWindowInfo({ window_id: 'w-b', status_rev: 5, status_boot: { origin: 7, seq: 2 } })
    );
    registry.setStatus('w-b', { status: 'idle' }, null, { origin: 7, seq: 2 });
    expect(entryOf(registry, 'w-b')?.statusRev).toBe(5);
    registry.setStatus('w-b', { status: 'interrupted' }, 3, { origin: 7, seq: 2 });
    expect(entryOf(registry, 'w-b')?.detected).toEqual({ status: 'idle' });

    registry.setStatus('w-b', { status: 'waiting_input' }, null, { origin: 7, seq: 3 });

    const nextBoot = entryOf(registry, 'w-b');
    expect(nextBoot?.detected).toEqual({ status: 'waiting_input' });
    expect(nextBoot?.statusRev).toBeNull();
    expect(nextBoot?.statusBoot).toEqual({ origin: 7, seq: 3 });
  });

  it('applies every observation of a window whose generation was never reported', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));

    registry.setStatus('w-a', { status: 'idle' }, null, null);
    expect(entryOf(registry, 'w-a')?.detected).toEqual({ status: 'idle' });

    registry.setStatus('w-a', { status: 'interrupted' }, 2, null);
    expect(entryOf(registry, 'w-a')?.detected).toEqual({ status: 'interrupted' });
  });

  it('compares generations per window, so a high generation on one window does not gate another', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 9 }));
    registry.openWindow(makeWindowInfo({ window_id: 'w-b', status_rev: 1 }));

    registry.setStatus('w-b', { status: 'idle' }, 2, null);

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

    // A later boot of the same series is newer whatever its generation.
    registry.openWindow(
      makeWindowInfo({ window_id: 'w-b', status_rev: 5, status_boot: { origin: 7, seq: 1 } })
    );
    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-b',
        detected: unknownPair('screen_lost'),
        status_rev: 1,
        status_boot: { origin: 7, seq: 2 },
      })
    );

    const later = entryOf(registry, 'w-b');
    expect(later?.detected).toEqual({ status: 'unknown', reason: 'screen_lost' });
    expect(later?.statusRev).toBe(1);
    expect(later?.statusBoot).toEqual({ origin: 7, seq: 2 });

    // Another series has no order against this one, so its observation is taken.
    registry.openWindow(
      makeWindowInfo({ window_id: 'w-c', status_rev: 5, status_boot: { origin: 7, seq: 3 } })
    );
    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-c',
        detected: unknownPair('screen_lost'),
        status_rev: 1,
        status_boot: { origin: 9, seq: 1 },
      })
    );

    const other = entryOf(registry, 'w-c');
    expect(other?.detected).toEqual({ status: 'unknown', reason: 'screen_lost' });
    expect(other?.statusRev).toBe(1);
    expect(other?.statusBoot).toEqual({ origin: 9, seq: 1 });
  });

  it('leaves the pair where it is for a snapshot taken at an older generation', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));
    registry.setStatus('w-a', { status: 'waiting_input' }, 7, null);

    registry.openWindow(
      makeWindowInfo({ window_id: 'w-a', detected: unknownPair('screen_lost'), status_rev: 6 })
    );

    const entry = entryOf(registry, 'w-a');
    expect(entry?.detected).toEqual({ status: 'waiting_input' });
    expect(entry?.statusRev).toBe(7);

    // An earlier boot of the same series is older whatever its generation.
    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-b',
        detected: { status: 'waiting_input' },
        status_rev: 7,
        status_boot: { origin: 7, seq: 2 },
      })
    );
    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-b',
        detected: unknownPair('screen_lost'),
        status_rev: 8,
        status_boot: { origin: 7, seq: 1 },
      })
    );

    const earlier = entryOf(registry, 'w-b');
    expect(earlier?.detected).toEqual({ status: 'waiting_input' });
    expect(earlier?.statusRev).toBe(7);
    expect(earlier?.statusBoot).toEqual({ origin: 7, seq: 2 });

    // Within one boot, an older generation is older.
    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-c',
        detected: { status: 'waiting_input' },
        status_rev: 7,
        status_boot: { origin: 7, seq: 2 },
      })
    );
    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-c',
        detected: unknownPair('screen_lost'),
        status_rev: 6,
        status_boot: { origin: 7, seq: 2 },
      })
    );

    const sameBoot = entryOf(registry, 'w-c');
    expect(sameBoot?.detected).toEqual({ status: 'waiting_input' });
    expect(sameBoot?.statusRev).toBe(7);
  });

  it('applies a snapshot taken at the same generation', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));
    registry.setStatus('w-a', { status: 'waiting_input' }, 7, null);

    registry.openWindow(
      makeWindowInfo({ window_id: 'w-a', detected: { status: 'idle' }, status_rev: 7 })
    );

    expect(entryOf(registry, 'w-a')?.detected).toEqual({ status: 'idle' });

    // The same generation within the same boot is applied too.
    registry.openWindow(
      makeWindowInfo({ window_id: 'w-b', status_rev: 7, status_boot: { origin: 7, seq: 2 } })
    );
    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-b',
        detected: { status: 'idle' },
        status_rev: 7,
        status_boot: { origin: 7, seq: 2 },
      })
    );

    expect(entryOf(registry, 'w-b')?.detected).toEqual({ status: 'idle' });
  });

  it('applies a snapshot that names no generation at all', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));
    registry.setStatus('w-a', { status: 'waiting_input' }, 7, null);

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
    registry.setStatus('w-a', { status: 'waiting_input' }, 7, null);

    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-a',
        name: 're\u2067named\u0000',
        detected: unknownPair('screen_lost'),
        room: { room_id: 'r-beta', name: 'oth\u202Der\u2060' },
        detection_warning: 'second\u200F warning\u009b',
        status_rev: 2,
      })
    );

    const entry = entryOf(registry, 'w-a');
    expect(entry?.name).toBe('re\u2067named\u0000');
    expect(entry?.displayName).toBe('re\\u{2067}named ');
    expect(entry?.room).toEqual({
      roomId: 'r-beta',
      displayName: 'oth\\u{202D}er\\u{2060}',
    });
    expect(entry?.displayDetectionWarning).toBe('second\\u{200F} warning ');
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
    registry.setStatus('w-a', { status: 'waiting_input' }, 7, null);

    registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 1 }));

    const entry = entryOf(registry, 'w-a');
    expect(entry?.room).toBeNull();
    expect(entry?.displayDetectionWarning).toBeNull();
    expect(entry?.detected).toEqual({ status: 'waiting_input' });
  });
});

describe('createWindowRegistry: which message retires an ended window', () => {
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

  type Message = 'push' | 'list';

  /** Hands one exited message about `w-a` to the registry through the shell's routing. */
  function deliver(registry: WindowRegistry, message: Message, exitCode: number | null): void {
    if (message === 'push') {
      const payload: StatusPayload = {
        window_id: 'w-a',
        detected: { status: 'exited' },
        exit_code: exitCode,
      };
      applyStatusPush(registry, payload);
      return;
    }
    // An ended window never counts as live.
    expect(
      applyListedWindow(
        registry,
        makeWindowInfo({ window_id: 'w-a', detected: { status: 'exited' }, exit_code: exitCode })
      )
    ).toBe(false);
  }

  it('counts a listed window that still runs as live', () => {
    const registry = createWindowRegistry();
    const live = applyListedWindow(
      registry,
      makeWindowInfo({ window_id: 'w-a', detected: { status: 'running' } })
    );
    expect(live).toBe(true);
    expect(get(registry).windows.map((entry) => entry.windowId)).toEqual(['w-a']);
  });

  /**
   * The code each update that took `w-a` off the list retired it with, for
   * `messages` delivered in order; `'absent'` when such an update names no code.
   */
  function retiredCodesOf(
    messages: readonly Message[],
    exitCode: number | null
  ): (number | null | 'absent')[] {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));
    registry.openWindow(makeWindowInfo({ window_id: 'w-b' }));
    const states = publishedStates(registry);
    for (const message of messages) {
      deliver(registry, message, exitCode);
    }
    expect(entryOf(registry, 'w-a')).toBeUndefined();
    expect(entryOf(registry, 'w-b')).toBeDefined();
    let listed = true;
    const codes: (number | null | 'absent')[] = [];
    for (const state of states) {
      const listedNow = state.windows.some((entry) => entry.windowId === 'w-a');
      if (listed && !listedNow) {
        const retired = state.retired.find((entry) => entry.windowId === 'w-a');
        codes.push(retired === undefined ? 'absent' : retired.exitCode);
      }
      listed = listedNow;
    }
    return codes;
  }

  it.each<[Message[]]>([[['push', 'list']], [['list', 'push']]])(
    'retires the window once with its code whichever of %j comes first',
    (messages) => {
      expect(retiredCodesOf(messages, 3)).toEqual([3]);
    }
  );

  it('the exited push alone retires the window, with the code it carries', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));
    const states = publishedStates(registry);

    deliver(registry, 'push', 3);

    expect(states).toHaveLength(1);
    expect(states[0]?.windows).toEqual([]);
    expect(states[0]?.retired).toEqual([{ windowId: 'w-a', exitCode: 3 }]);
  });

  // -1 is a real exit code, never a stand-in for one that could not be determined.
  it.each<[Message]>([['list'], ['push']])('a -1 the %s carries is retired as -1', (message) => {
    expect(retiredCodesOf([message], -1)).toEqual([-1]);
  });

  it('an exited push whose code could not be determined retires the window with none', () => {
    expect(retiredCodesOf(['push'], null)).toEqual([null]);
  });

  it('a status other than exited retires nothing', () => {
    const running: StatusPayload = {
      window_id: 'w-a',
      detected: { status: 'running' },
      exit_code: null,
    };

    expect(retirementFromStatus(running)).toBeNull();
    expect(retirementFromList(makeWindowInfo({ window_id: 'w-a' }))).toBeNull();
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

    registry.setStatus('w-a', { status: 'running' }, 2, null);
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

    registry.setStatus('w-a', { status: 'idle' }, 3, null);
    const entry = entryOf(registry, 'w-a');
    expect(pairHoldsTogether(entry?.detected)).toBe(true);
    expect(entry?.detected).toEqual({ status: 'idle' });
  });

  it('holds together when a discarded older observation carried a reason', () => {
    const registry = createWindowRegistry();
    registry.openWindow(makeWindowInfo({ window_id: 'w-a' }));
    registry.setStatus('w-a', { status: 'running' }, 5, null);

    registry.setStatus('w-a', unknownPair('screen_lost'), 1, null);

    const entry = entryOf(registry, 'w-a');
    expect(pairHoldsTogether(entry?.detected)).toBe(true);
    expect(entry?.detected).toEqual({ status: 'running' });
  });
});

/** The states a report mark reads as at the boundary. */
type ReportStateValue = 'standing' | 'instruction' | 'no_reader' | 'unknown';

/**
 * Within one `seq` a mark only moves up this order, written here as a literal
 * table rather than read from production: `standing` < `no_reader` = `unknown`
 * < `instruction`.
 */
const STATE_RANK: Record<ReportStateValue, number> = {
  standing: 0,
  no_reader: 1,
  unknown: 1,
  instruction: 2,
};

const ALL_STATES = Object.keys(STATE_RANK) as ReportStateValue[];

/**
 * A task window's report mark is ordered on its own key, apart from the status
 * pair: within one boot a larger `seq` is newer, and for the same `seq` the
 * state only moves up `STATE_RANK`. So the order the two routes arrive in must
 * never move a mark back to a reason it already left.
 */
describe('createWindowRegistry: the task report mark merges on its own key', () => {
  const BOOT = { origin: 7, seq: 1 };

  const SAME_SEQ_PAIRS = ALL_STATES.flatMap((held) =>
    ALL_STATES.map((arriving) => [held, arriving] as const)
  );

  it('holding a listed mark, a pushed mark of the same seq replaces it only when it ranks higher', () => {
    for (const [held, arriving] of SAME_SEQ_PAIRS) {
      const registry = createWindowRegistry();
      registry.openWindow(
        makeWindowInfo({
          window_id: 'w-a',
          detected: { status: 'waiting_input' },
          status_rev: 3,
          status_boot: BOOT,
          task_report: { seq: 1, state: held },
        })
      );
      expect(entryOf(registry, 'w-a')?.taskReport, `listed ${held}`).toEqual({
        seq: 1,
        state: held,
      });

      registry.setStatus('w-a', { status: 'waiting_input' }, 3, BOOT, { seq: 1, state: arriving });

      const expected = STATE_RANK[arriving] > STATE_RANK[held] ? arriving : held;
      expect(entryOf(registry, 'w-a')?.taskReport, `listed ${held}, pushed ${arriving}`).toEqual({
        seq: 1,
        state: expected,
      });
    }
  });

  it('holding a pushed mark, a listed mark of the same seq replaces it only when it ranks higher', () => {
    for (const [held, arriving] of SAME_SEQ_PAIRS) {
      const registry = createWindowRegistry();
      registry.openWindow(makeWindowInfo({ window_id: 'w-a', status_rev: 2, status_boot: BOOT }));
      registry.setStatus('w-a', { status: 'waiting_input' }, 3, BOOT, { seq: 1, state: held });
      expect(entryOf(registry, 'w-a')?.taskReport, `pushed ${held}`).toEqual({
        seq: 1,
        state: held,
      });

      registry.openWindow(
        makeWindowInfo({
          window_id: 'w-a',
          detected: { status: 'waiting_input' },
          status_rev: 3,
          status_boot: BOOT,
          task_report: { seq: 1, state: arriving },
        })
      );

      const expected = STATE_RANK[arriving] > STATE_RANK[held] ? arriving : held;
      expect(entryOf(registry, 'w-a')?.taskReport, `pushed ${held}, listed ${arriving}`).toEqual({
        seq: 1,
        state: expected,
      });
    }
  });

  it('takes a mark of a larger seq on either route, and never one of a smaller seq', () => {
    const registry = createWindowRegistry();
    for (const windowId of ['w-list', 'w-push']) {
      registry.openWindow(
        makeWindowInfo({
          window_id: windowId,
          status_rev: 3,
          status_boot: BOOT,
          task_report: { seq: 1, state: 'instruction' },
        })
      );
    }

    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-list',
        detected: { status: 'waiting_input' },
        status_rev: 5,
        status_boot: BOOT,
        task_report: { seq: 2, state: 'standing' },
      })
    );
    registry.setStatus('w-push', { status: 'waiting_input' }, 5, BOOT, {
      seq: 2,
      state: 'standing',
    });

    expect(entryOf(registry, 'w-list')?.taskReport).toEqual({ seq: 2, state: 'standing' });
    expect(entryOf(registry, 'w-push')?.taskReport).toEqual({ seq: 2, state: 'standing' });

    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-list',
        detected: { status: 'waiting_input' },
        status_rev: 6,
        status_boot: BOOT,
        task_report: { seq: 1, state: 'instruction' },
      })
    );
    registry.setStatus('w-push', { status: 'waiting_input' }, 6, BOOT, {
      seq: 1,
      state: 'instruction',
    });

    expect(entryOf(registry, 'w-list')?.taskReport).toEqual({ seq: 2, state: 'standing' });
    expect(entryOf(registry, 'w-push')?.taskReport).toEqual({ seq: 2, state: 'standing' });
  });

  it('takes the mark of a push whose status pair is older than the one held', () => {
    const registry = createWindowRegistry();
    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-a',
        detected: { status: 'waiting_input' },
        status_rev: 8,
        status_boot: BOOT,
        task_report: { seq: 1, state: 'standing' },
      })
    );

    registry.setStatus('w-a', { status: 'running' }, 7, BOOT, { seq: 1, state: 'instruction' });

    const entry = entryOf(registry, 'w-a');
    expect(entry?.detected).toEqual({ status: 'waiting_input' });
    expect(entry?.statusRev).toBe(8);
    expect(entry?.taskReport).toEqual({ seq: 1, state: 'instruction' });
  });

  it('takes the arriving observation whole once the boot changes, a missing mark clearing the one held', () => {
    const registry = createWindowRegistry();
    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-a',
        status_rev: 9,
        status_boot: BOOT,
        task_report: { seq: 5, state: 'instruction' },
      })
    );

    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-a',
        detected: { status: 'waiting_input' },
        status_rev: 1,
        status_boot: { origin: 7, seq: 2 },
        task_report: { seq: 1, state: 'standing' },
      })
    );
    expect(entryOf(registry, 'w-a')?.taskReport).toEqual({ seq: 1, state: 'standing' });

    registry.setStatus('w-a', { status: 'waiting_input' }, 1, { origin: 9, seq: 1 });

    expect(entryOf(registry, 'w-a')?.taskReport).toBeUndefined();
  });

  it('keeps the mark it holds when an observation of the same boot carries none', () => {
    const registry = createWindowRegistry();
    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-a',
        detected: { status: 'waiting_input' },
        status_rev: 3,
        status_boot: BOOT,
        task_report: { seq: 1, state: 'standing' },
      })
    );

    registry.setStatus('w-a', { status: 'waiting_input' }, 4, BOOT);
    expect(entryOf(registry, 'w-a')?.taskReport).toEqual({ seq: 1, state: 'standing' });

    registry.openWindow(
      makeWindowInfo({
        window_id: 'w-a',
        detected: { status: 'waiting_input' },
        status_rev: 5,
        status_boot: BOOT,
      })
    );
    expect(entryOf(registry, 'w-a')?.taskReport).toEqual({ seq: 1, state: 'standing' });
  });
});

/**
 * The count a window's waiting-messages mark shows, read from the daemon's
 * window record. Zero and a record from a daemon that predates the field both
 * show no mark.
 */
describe('queuedCountOf', () => {
  // Loaded per case so each case fails on its own while the export is missing.
  async function loadQueuedCountOf(): Promise<
    (typeof import('./window-registry'))['queuedCountOf']
  > {
    const module = await import('./window-registry');
    return module.queuedCountOf;
  }

  it('gives the count when messages wait in the window', async () => {
    const queuedCountOf = await loadQueuedCountOf();

    expect(queuedCountOf(makeWindowInfo({ queued_count: 1 }))).toBe(1);
    expect(queuedCountOf(makeWindowInfo({ queued_count: 7 }))).toBe(7);
  });

  it('shows no mark when nothing waits', async () => {
    const queuedCountOf = await loadQueuedCountOf();

    expect(queuedCountOf(makeWindowInfo({ queued_count: 0 }))).toBeNull();
  });

  it('shows no mark when the daemon does not report the count', async () => {
    const queuedCountOf = await loadQueuedCountOf();

    expect(queuedCountOf(makeWindowInfo())).toBeNull();
  });
});

describe('the waiting-messages mark tooltip', () => {
  it('says how many messages wait and when they are handed over, plural above one', () => {
    expect(resolveMessages().formats.queuedMessages(2)).toBe(
      '2 queued messages — delivered on its next turn'
    );
  });

  it('says it in the singular for one message', () => {
    expect(resolveMessages().formats.queuedMessages(1)).toBe(
      '1 queued message — delivered on its next turn'
    );
  });
});
