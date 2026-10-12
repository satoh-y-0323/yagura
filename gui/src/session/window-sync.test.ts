import { describe, it, expect, vi } from 'vitest';

import { syncWindows, type WindowSyncDeps } from './window-sync';

function makeDeps(overrides: Partial<WindowSyncDeps> = {}): WindowSyncDeps {
  return {
    openPane: vi.fn().mockResolvedValue(undefined),
    closePane: vi.fn(),
    onError: vi.fn(),
    ...overrides,
  };
}

describe('syncWindows: opens a pane for every window', () => {
  it('calls openPane once per daemon window id', async () => {
    const deps = makeDeps();

    await syncWindows(['w-a', 'w-b', 'w-c'], [], deps);

    expect(deps.openPane).toHaveBeenCalledTimes(3);
    expect(deps.openPane).toHaveBeenNthCalledWith(1, 'w-a');
    expect(deps.openPane).toHaveBeenNthCalledWith(2, 'w-b');
    expect(deps.openPane).toHaveBeenNthCalledWith(3, 'w-c');
  });
});

describe('syncWindows: opens panes sequentially', () => {
  it('does not call openPane for the next window until the previous one resolves', async () => {
    let resolveFirst: (() => void) | undefined;
    const firstGate = new Promise<void>((resolve) => {
      resolveFirst = resolve;
    });
    const openPane = vi.fn().mockImplementation((windowId: string) => {
      if (windowId === 'w-a') {
        return firstGate;
      }
      return Promise.resolve();
    });
    const deps = makeDeps({ openPane });

    const syncPromise = syncWindows(['w-a', 'w-b'], [], deps);

    // Let any already-queued microtasks run without resolving the gate.
    await Promise.resolve();
    await Promise.resolve();

    expect(openPane).toHaveBeenCalledTimes(1);
    expect(openPane).toHaveBeenCalledWith('w-a');

    resolveFirst?.();
    await syncPromise;

    expect(openPane).toHaveBeenCalledTimes(2);
    expect(openPane).toHaveBeenNthCalledWith(2, 'w-b');
  });
});

describe('syncWindows: closes stale panes', () => {
  it('calls closePane for windows that disappeared from the daemon list', async () => {
    const deps = makeDeps();

    await syncWindows(['w-a'], ['w-a', 'w-b'], deps);

    expect(deps.closePane).toHaveBeenCalledTimes(1);
    expect(deps.closePane).toHaveBeenCalledWith('w-b');
  });
});

describe('syncWindows: one failure does not stop the others', () => {
  it('continues opening remaining windows and reports the failure via onError', async () => {
    const failure = new Error('attach failed');
    const openPane = vi.fn().mockImplementation((windowId: string) => {
      if (windowId === 'w-b') {
        return Promise.reject(failure);
      }
      return Promise.resolve();
    });
    const deps = makeDeps({ openPane });

    await syncWindows(['w-a', 'w-b', 'w-c'], [], deps);

    expect(openPane).toHaveBeenCalledTimes(3);
    expect(deps.onError).toHaveBeenCalledTimes(1);
    expect(deps.onError).toHaveBeenCalledWith('w-b', failure);
  });
});

describe('syncWindows: idempotent', () => {
  it('does not call openPane again when applying the same snapshot twice', async () => {
    const deps = makeDeps();

    await syncWindows(['w-a', 'w-b'], [], deps);
    expect(deps.openPane).toHaveBeenCalledTimes(2);

    await syncWindows(['w-a', 'w-b'], ['w-a', 'w-b'], deps);
    expect(deps.openPane).toHaveBeenCalledTimes(2);
  });
});

describe('syncWindows: returns the applied plan', () => {
  it('resolves to the PanePlan it applied, matching the closePane/openPane calls and their order', async () => {
    const deps = makeDeps();

    const plan = await syncWindows(['w-a', 'w-c'], ['w-a', 'w-b'], deps);

    expect(plan).toEqual({ toOpen: ['w-c'], toClose: ['w-b'] });
    expect(deps.closePane).toHaveBeenCalledTimes(1);
    expect(deps.closePane).toHaveBeenCalledWith('w-b');
    expect(deps.openPane).toHaveBeenCalledTimes(1);
    expect(deps.openPane).toHaveBeenCalledWith('w-c');
  });
});

describe('syncWindows: closePane throwing does not change the returned plan', () => {
  it('routes the thrown error to onError and still resolves to the plan it applied', async () => {
    const failure = new Error('teardown failed');
    const closePane = vi.fn().mockImplementation((windowId: string) => {
      if (windowId === 'w-b') {
        throw failure;
      }
    });
    const deps = makeDeps({ closePane });

    const plan = await syncWindows(['w-a'], ['w-a', 'w-b'], deps);

    expect(plan).toEqual({ toOpen: [], toClose: ['w-b'] });
    expect(deps.onError).toHaveBeenCalledTimes(1);
    expect(deps.onError).toHaveBeenCalledWith('w-b', failure);
  });
});
