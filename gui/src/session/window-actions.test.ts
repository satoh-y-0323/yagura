/**
 * `window-actions.ts` is the single execution path for close / interrupt / rename /
 * goTo / minimize / maximize; move and resize stay out because they are continuous,
 * confirmation-free coordinate updates `FloatWindow` writes to the registry directly.
 * Deps are injected at construction, and the confirm port is optional: without it,
 * destructive operations must not reach execution (fail-closed).
 */
import { describe, it, expect, vi, beforeEach, type Mock } from 'vitest';

import type { WindowStatus } from '../ipc/session-boundary';
import type { WindowEntry } from '../stores/window-registry';
import * as confirmTargetModule from './confirm-target';
import {
  createWindowActions,
  type ConfirmPort,
  type WindowActions,
  type WindowActionsIpcClient,
  type WindowActionsRegistry,
} from './window-actions';

vi.mock('./confirm-target', async () => {
  const actual = await vi.importActual<typeof confirmTargetModule>('./confirm-target');
  return {
    ...actual,
    resolveConfirmTarget: vi.fn(actual.resolveConfirmTarget),
  };
});

function makeEntry(overrides: Partial<WindowEntry> & { status?: WindowStatus } = {}): WindowEntry {
  const { status = 'running', ...rest } = overrides;
  return {
    windowId: 'w-1',
    name: 'w-1',
    displayName: 'w-1',
    vendor: null,
    displayVendor: null,
    cwd: 'C:\\Users\\dev\\project',
    displayCwd: 'C:\\Users\\dev\\project',
    detected: status === 'unknown' ? { status, reason: null } : { status },
    statusRev: null,
    statusBoot: null,
    exitCode: null,
    room: null,
    detectionWarning: null,
    displayDetectionWarning: null,
    mcpConnected: null,
    statusReportsMissing: null,
    display: {
      position: null,
      size: null,
      zOrder: 0,
      isMinimized: false,
      isMaximized: false,
      restoreBounds: null,
    },
    ...rest,
  };
}

/**
 * Mocks use vitest's `Mock<T>`: the untyped `ReturnType<typeof vi.fn>` widens to
 * `Mock<Procedure | Constructable>` and is not assignable back to a specific
 * signature. A mapped type, not `extends`, which rejects re-declaring members (TS2430).
 */
type MockRegistry = { [K in keyof WindowActionsRegistry]: Mock<WindowActionsRegistry[K]> };
type MockIpc = { [K in keyof WindowActionsIpcClient]: Mock<WindowActionsIpcClient[K]> };
type MockConfirmPort = Mock<ConfirmPort>;

function makeRegistry(): MockRegistry {
  return {
    renameWindow: vi.fn<WindowActionsRegistry['renameWindow']>(),
    bringWindowToFront: vi.fn<WindowActionsRegistry['bringWindowToFront']>(),
    setWindowMinimized: vi.fn<WindowActionsRegistry['setWindowMinimized']>(),
    setWindowMaximized: vi.fn<WindowActionsRegistry['setWindowMaximized']>(),
  };
}

function makeIpc(): MockIpc {
  return {
    invokeCloseWindow: vi
      .fn<WindowActionsIpcClient['invokeCloseWindow']>()
      .mockResolvedValue('closed'),
    invokeInterruptWindow: vi
      .fn<WindowActionsIpcClient['invokeInterruptWindow']>()
      .mockResolvedValue(undefined),
    invokeRenameWindow: vi
      .fn<WindowActionsIpcClient['invokeRenameWindow']>()
      .mockResolvedValue('stored-name'),
  };
}

/** A confirm port that always approves; most tests do not care about the dialog itself. */
function approvingConfirmPort(): MockConfirmPort {
  return vi.fn<ConfirmPort>().mockResolvedValue(true);
}

type MockRemoveWindow = Mock<(windowId: string) => void>;

function makeRemoveWindow(): MockRemoveWindow {
  return vi.fn<(windowId: string) => void>();
}

type MockSelectWindow = Mock<(windowId: string) => void>;

function buildActions(
  overrides: {
    registry?: MockRegistry;
    ipc?: MockIpc;
    confirmPort?: ConfirmPort;
    removeWindow?: (windowId: string) => void;
    workMode?: 'canvas' | 'focus';
    closeRequested?: (windowId: string) => void;
    closeFailed?: (windowId: string) => void;
  } = {}
): {
  actions: WindowActions;
  registry: MockRegistry;
  ipc: MockIpc;
  selectWindow: MockSelectWindow;
} {
  const registry = overrides.registry ?? makeRegistry();
  const ipc = overrides.ipc ?? makeIpc();
  const selectWindow: MockSelectWindow = vi.fn<(windowId: string) => void>();
  const workMode = overrides.workMode ?? 'canvas';
  const actions = createWindowActions({
    registry,
    ipc,
    selectWindow,
    workMode: () => workMode,
    // exactOptionalPropertyTypes: passing `confirmPort: undefined` is a different
    // (rejected) shape from omitting the key entirely.
    ...(overrides.confirmPort !== undefined ? { confirmPort: overrides.confirmPort } : {}),
    ...(overrides.removeWindow !== undefined ? { removeWindow: overrides.removeWindow } : {}),
    ...(overrides.closeRequested !== undefined ? { closeRequested: overrides.closeRequested } : {}),
    ...(overrides.closeFailed !== undefined ? { closeFailed: overrides.closeFailed } : {}),
  });
  return { actions, registry, ipc, selectWindow };
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe('window-actions: close honors resolveConfirmTarget instead of re-implementing it', () => {
  it('allows close for a window in unknown status (close stays available)', async () => {
    const confirmPort = approvingConfirmPort();
    const { actions, ipc } = buildActions({ confirmPort });

    const done = actions.close(makeEntry({ status: 'unknown' }));
    expect(confirmPort, 'before the returned promise is awaited').toHaveBeenCalledWith(
      'close',
      expect.objectContaining({ windowId: 'w-1' })
    );
    await done;

    expect(ipc.invokeCloseWindow).toHaveBeenCalledWith('w-1');
  });

  it('calls resolveConfirmTarget with the operation and the window status, not a local rule', async () => {
    const confirmPort = approvingConfirmPort();
    const { actions } = buildActions({ confirmPort });
    const entry = makeEntry({ status: 'waiting_approval' });

    await actions.close(entry);

    expect(confirmTargetModule.resolveConfirmTarget).toHaveBeenCalledWith(
      'waiting_approval',
      'close'
    );
  });

  it('does not execute when resolveConfirmTarget reports the operation is not allowed', async () => {
    vi.mocked(confirmTargetModule.resolveConfirmTarget).mockReturnValueOnce({
      allowed: false,
      requiresConfirmation: false,
      reason: 'unknown',
    });
    const confirmPort = approvingConfirmPort();
    const { actions, ipc } = buildActions({ confirmPort });

    await actions.close(makeEntry());

    expect(confirmPort).not.toHaveBeenCalled();
    expect(ipc.invokeCloseWindow).not.toHaveBeenCalled();
  });

  it('skips the confirm port when resolveConfirmTarget reports no confirmation is required', async () => {
    vi.mocked(confirmTargetModule.resolveConfirmTarget).mockReturnValueOnce({
      allowed: true,
      requiresConfirmation: false,
    });
    const confirmPort = approvingConfirmPort();
    const { actions, ipc } = buildActions({ confirmPort });

    await actions.close(makeEntry());

    expect(confirmPort).not.toHaveBeenCalled();
    expect(ipc.invokeCloseWindow).toHaveBeenCalledWith('w-1');
  });
});

describe('window-actions: close removes the pane once the daemon has confirmed it', () => {
  it('removes the pane once the close has been carried out', async () => {
    const removeWindow = makeRemoveWindow();
    const { actions, ipc } = buildActions({
      confirmPort: approvingConfirmPort(),
      removeWindow,
    });

    await actions.close(makeEntry({ status: 'running' }));

    expect(ipc.invokeCloseWindow).toHaveBeenCalledWith('w-1');
    expect(removeWindow).toHaveBeenCalledTimes(1);
    expect(removeWindow).toHaveBeenCalledWith('w-1');
  });

  it('removes the pane only after the daemon has answered the close, not alongside it', async () => {
    const ipc = makeIpc();
    let closeAnswered = false;
    ipc.invokeCloseWindow.mockImplementation(async () => {
      await Promise.resolve();
      closeAnswered = true;
      return 'closed';
    });
    const removeWindow = makeRemoveWindow();
    let closeWasAnsweredFirst = false;
    removeWindow.mockImplementation(() => {
      closeWasAnsweredFirst = closeAnswered;
    });
    const { actions } = buildActions({
      ipc,
      confirmPort: approvingConfirmPort(),
      removeWindow,
    });

    await actions.close(makeEntry({ status: 'running' }));

    expect(removeWindow).toHaveBeenCalledTimes(1);
    expect(closeWasAnsweredFirst).toBe(true);
  });

  it('also removes the pane for a window in unknown status, where close stays allowed', async () => {
    const removeWindow = makeRemoveWindow();
    const { actions } = buildActions({
      confirmPort: approvingConfirmPort(),
      removeWindow,
    });

    await actions.close(makeEntry({ status: 'unknown' }));

    expect(removeWindow).toHaveBeenCalledTimes(1);
  });

  it('does not remove the pane when the confirm port declines: nothing was closed', async () => {
    const removeWindow = makeRemoveWindow();
    const { actions, ipc } = buildActions({
      confirmPort: vi.fn<ConfirmPort>().mockResolvedValue(false),
      removeWindow,
    });

    await actions.close(makeEntry({ status: 'running' }));

    expect(ipc.invokeCloseWindow).not.toHaveBeenCalled();
    expect(removeWindow).not.toHaveBeenCalled();
  });

  it('does not remove the pane when resolveConfirmTarget reports the close is not allowed', async () => {
    vi.mocked(confirmTargetModule.resolveConfirmTarget).mockReturnValueOnce({
      allowed: false,
      requiresConfirmation: false,
      reason: 'unknown',
    });
    const removeWindow = makeRemoveWindow();
    const { actions } = buildActions({
      confirmPort: approvingConfirmPort(),
      removeWindow,
    });

    await actions.close(makeEntry({ status: 'running' }));

    expect(removeWindow).not.toHaveBeenCalled();
  });

  it('does not remove the pane when no confirm port is registered (fail-closed)', async () => {
    const removeWindow = makeRemoveWindow();
    const { actions } = buildActions({ removeWindow });

    await actions.close(makeEntry({ status: 'running' }));

    expect(removeWindow).not.toHaveBeenCalled();
  });

  it('does not remove the pane when the daemon refuses the close', async () => {
    const ipc = makeIpc();
    ipc.invokeCloseWindow.mockRejectedValue(new Error('daemon refused the close'));
    const removeWindow = makeRemoveWindow();
    const { actions } = buildActions({
      ipc,
      confirmPort: approvingConfirmPort(),
      removeWindow,
    });

    await actions.close(makeEntry({ status: 'running' })).catch(() => undefined);

    expect(removeWindow).not.toHaveBeenCalled();
  });

  it('still propagates a refused close to the caller, which is what raises the banner', async () => {
    const ipc = makeIpc();
    ipc.invokeCloseWindow.mockRejectedValue(new Error('daemon refused the close'));
    const { actions } = buildActions({
      ipc,
      confirmPort: approvingConfirmPort(),
      removeWindow: makeRemoveWindow(),
    });

    await expect(actions.close(makeEntry({ status: 'running' }))).rejects.toThrow(
      'daemon refused the close'
    );
  });

  it('closes as before when no removeWindow hook is registered', async () => {
    const { actions, ipc } = buildActions({ confirmPort: approvingConfirmPort() });

    await actions.close(makeEntry({ status: 'running' }));

    expect(ipc.invokeCloseWindow).toHaveBeenCalledWith('w-1');
  });

  it('does not remove the pane for an interrupt: its status arrives through its own event', async () => {
    const removeWindow = makeRemoveWindow();
    const { actions, ipc } = buildActions({
      confirmPort: approvingConfirmPort(),
      removeWindow,
    });

    await actions.interrupt(makeEntry({ status: 'running' }));

    expect(ipc.invokeInterruptWindow).toHaveBeenCalledWith('w-1');
    expect(removeWindow).not.toHaveBeenCalled();
  });

  it('does not remove the pane for a rename either', async () => {
    const removeWindow = makeRemoveWindow();
    const { actions } = buildActions({
      confirmPort: approvingConfirmPort(),
      removeWindow,
    });

    await actions.rename(makeEntry(), 'new-name');

    expect(removeWindow).not.toHaveBeenCalled();
  });
});

describe('window-actions: interrupt is suppressed for unknown windows, offered otherwise', () => {
  it('does not offer interrupt for a window in unknown status', async () => {
    const confirmPort = approvingConfirmPort();
    const { actions, ipc } = buildActions({ confirmPort });

    await actions.interrupt(makeEntry({ status: 'unknown' }));

    expect(confirmPort).not.toHaveBeenCalled();
    expect(ipc.invokeInterruptWindow).not.toHaveBeenCalled();
  });

  it('does not offer interrupt for a window in exited status', async () => {
    const confirmPort = approvingConfirmPort();
    const { actions, ipc } = buildActions({ confirmPort });

    await actions.interrupt(makeEntry({ status: 'exited' }));

    expect(ipc.invokeInterruptWindow).not.toHaveBeenCalled();
  });

  it('offers interrupt with a confirmation for a running window', async () => {
    const confirmPort = approvingConfirmPort();
    const { actions, ipc } = buildActions({ confirmPort });

    const done = actions.interrupt(makeEntry({ status: 'running' }));
    expect(confirmPort, 'before the returned promise is awaited').toHaveBeenCalledWith(
      'interrupt',
      expect.objectContaining({ windowId: 'w-1' })
    );
    await done;

    expect(confirmPort).toHaveBeenCalledWith(
      'interrupt',
      expect.objectContaining({ windowId: 'w-1' })
    );
    expect(ipc.invokeInterruptWindow).toHaveBeenCalledWith('w-1');
  });
});

describe('window-actions: no destructive operation reaches execution without the confirm port', () => {
  it('does not close when the confirm port is not registered (fail-closed)', async () => {
    const { actions, ipc } = buildActions(); // no confirmPort in deps

    await actions.close(makeEntry({ status: 'running' }));

    expect(ipc.invokeCloseWindow).not.toHaveBeenCalled();
  });

  it('does not interrupt when the confirm port is not registered (fail-closed)', async () => {
    const { actions, ipc } = buildActions(); // no confirmPort in deps

    await actions.interrupt(makeEntry({ status: 'running' }));

    expect(ipc.invokeInterruptWindow).not.toHaveBeenCalled();
  });

  it('does not close when the confirm port declines', async () => {
    const confirmPort = vi.fn<ConfirmPort>().mockResolvedValue(false);
    const { actions, ipc } = buildActions({ confirmPort });

    await actions.close(makeEntry({ status: 'running' }));

    expect(ipc.invokeCloseWindow).not.toHaveBeenCalled();
  });

  it('does not interrupt when the confirm port declines', async () => {
    const confirmPort = vi.fn<ConfirmPort>().mockResolvedValue(false);
    const { actions, ipc } = buildActions({ confirmPort });

    await actions.interrupt(makeEntry({ status: 'running' }));

    expect(ipc.invokeInterruptWindow).not.toHaveBeenCalled();
  });
});

describe('window-actions: rename needs no confirmation and propagates failures', () => {
  it('renames through the ipc client without going through the confirm port', async () => {
    const confirmPort = approvingConfirmPort();
    const { actions, ipc } = buildActions({ confirmPort });

    await actions.rename(makeEntry(), 'new-name');

    expect(confirmPort).not.toHaveBeenCalled();
    expect(ipc.invokeRenameWindow).toHaveBeenCalledWith('w-1', 'new-name');
  });

  it('renames even when no confirm port is registered', async () => {
    const { actions, ipc } = buildActions(); // no confirmPort in deps

    await actions.rename(makeEntry(), 'new-name');

    expect(ipc.invokeRenameWindow).toHaveBeenCalledWith('w-1', 'new-name');
  });

  it('writes the name the daemon actually stored back to the registry', async () => {
    const ipc = makeIpc();
    ipc.invokeRenameWindow.mockResolvedValue('normalized-name');
    const { actions, registry } = buildActions({ ipc });

    await actions.rename(makeEntry(), 'requested-name');

    expect(registry.renameWindow).toHaveBeenCalledWith('w-1', 'normalized-name');
  });

  it('propagates an ipc failure to the caller instead of swallowing it', async () => {
    const ipc = makeIpc();
    ipc.invokeRenameWindow.mockRejectedValue(new Error('daemon rejected the name'));
    const { actions } = buildActions({ ipc });

    await expect(actions.rename(makeEntry(), 'bad-name')).rejects.toThrow(
      'daemon rejected the name'
    );
  });

  it('does not update the registry when the ipc call fails', async () => {
    const ipc = makeIpc();
    ipc.invokeRenameWindow.mockRejectedValue(new Error('daemon rejected the name'));
    const { actions, registry } = buildActions({ ipc });

    await actions.rename(makeEntry(), 'bad-name').catch(() => undefined);

    expect(registry.renameWindow).not.toHaveBeenCalled();
  });
});

describe('window-actions: goTo in Canvas brings the window to front and restores it if minimized', () => {
  it('brings a normal window to front by delegating to the registry, not recomputing zOrder', () => {
    const { actions, registry } = buildActions();

    actions.goTo(makeEntry({ status: 'running' }));

    expect(registry.bringWindowToFront).toHaveBeenCalledTimes(1);
    expect(registry.bringWindowToFront).toHaveBeenCalledWith('w-1');
    expect(registry.setWindowMinimized).not.toHaveBeenCalled();
  });

  it('restores a minimized window and still brings it to front', () => {
    const { actions, registry } = buildActions();

    actions.goTo(makeEntry({ display: { ...makeEntry().display, isMinimized: true } }));

    expect(registry.setWindowMinimized).toHaveBeenCalledWith('w-1', false);
    expect(registry.bringWindowToFront).toHaveBeenCalledWith('w-1');
  });
});

describe('window-actions: goTo in Focus only chooses the window and never changes the placement', () => {
  const placed = (isMinimized: boolean, isMaximized: boolean): WindowEntry =>
    makeEntry({ display: { ...makeEntry().display, isMinimized, isMaximized } });

  it.each([
    ['a plain window', placed(false, false)],
    ['a minimized window', placed(true, false)],
    ['a maximized window', placed(false, true)],
  ])('selects %s through the shell and writes no placement', (_label, entry) => {
    const { actions, registry, selectWindow } = buildActions({ workMode: 'focus' });

    actions.goTo(entry);

    expect(selectWindow).toHaveBeenCalledTimes(1);
    expect(selectWindow).toHaveBeenCalledWith('w-1');
    expect(registry.setWindowMinimized).not.toHaveBeenCalled();
    expect(registry.setWindowMaximized).not.toHaveBeenCalled();
    expect(registry.bringWindowToFront).not.toHaveBeenCalled();
  });
});

describe('window-actions: minimize and maximize delegate to the registry without touching zOrder', () => {
  it('minimizes a window through the registry mutator', () => {
    const { actions, registry } = buildActions();

    actions.minimize(makeEntry());

    expect(registry.setWindowMinimized).toHaveBeenCalledWith('w-1', true);
    expect(registry.bringWindowToFront).not.toHaveBeenCalled();
  });

  it('toggles maximize on for a window that is not maximized', () => {
    const { actions, registry } = buildActions();

    actions.maximize(makeEntry({ display: { ...makeEntry().display, isMaximized: false } }));

    expect(registry.setWindowMaximized).toHaveBeenCalledWith('w-1', true);
    expect(registry.bringWindowToFront).not.toHaveBeenCalled();
  });

  it('toggles maximize off for a window that is already maximized', () => {
    const { actions, registry } = buildActions();

    actions.maximize(makeEntry({ display: { ...makeEntry().display, isMaximized: true } }));

    expect(registry.setWindowMaximized).toHaveBeenCalledWith('w-1', false);
  });
});

describe('window-actions: a pane-removal failure is absorbed here, not left for the provider to guard', () => {
  // A throwing removeWindow must not turn into a rejected close, and the absorption
  // must stay silent: the provider is the one that logs.
  it('does not reject the close and does not touch the console when removeWindow throws', async () => {
    const removeWindow = vi.fn<(windowId: string) => void>().mockImplementation(() => {
      throw new Error('pane removal failed');
    });
    const consoleMethods = ['log', 'warn', 'error', 'info', 'debug'] as const;
    const consoleSpies = consoleMethods.map((method) =>
      vi.spyOn(console, method).mockImplementation(() => undefined)
    );
    const { actions, ipc } = buildActions({
      confirmPort: approvingConfirmPort(),
      removeWindow,
    });

    await expect(actions.close(makeEntry({ status: 'running' }))).resolves.toBeUndefined();

    // Baseline: removeWindow really ran and threw, so the assertions below are not
    // vacuous.
    expect(ipc.invokeCloseWindow).toHaveBeenCalledWith('w-1');
    expect(removeWindow).toHaveBeenCalledTimes(1);
    for (const spy of consoleSpies) {
      expect(spy).not.toHaveBeenCalled();
    }

    consoleSpies.forEach((spy) => {
      spy.mockRestore();
    });
  });
});

describe('window-actions: close tells the shell state who is closing the window', () => {
  it('says so after the confirmation and before the close reaches the daemon', async () => {
    const calls: string[] = [];
    const ipc = makeIpc();
    ipc.invokeCloseWindow.mockImplementation(() => {
      calls.push('invokeCloseWindow');
      return Promise.resolve('closed');
    });
    const confirmPort = vi.fn<ConfirmPort>().mockImplementation(() => {
      calls.push('confirm');
      return Promise.resolve(true);
    });
    const closeFailed = vi.fn<(windowId: string) => void>();
    const { actions } = buildActions({
      ipc,
      confirmPort,
      closeRequested: (windowId) => calls.push(`closeRequested:${windowId}`),
      closeFailed,
    });

    await actions.close(makeEntry({ status: 'running' }));

    expect(calls).toEqual(['confirm', 'closeRequested:w-1', 'invokeCloseWindow']);
    expect(closeFailed).not.toHaveBeenCalled();
  });

  it('takes the window back out when the daemon refuses the close, and still rejects', async () => {
    const ipc = makeIpc();
    ipc.invokeCloseWindow.mockRejectedValue(new Error('daemon refused the close'));
    const closeRequested = vi.fn<(windowId: string) => void>();
    const closeFailed = vi.fn<(windowId: string) => void>();
    const { actions } = buildActions({
      ipc,
      confirmPort: approvingConfirmPort(),
      closeRequested,
      closeFailed,
    });

    await expect(actions.close(makeEntry({ status: 'running' }))).rejects.toThrow(
      'daemon refused the close'
    );

    expect(closeRequested).toHaveBeenCalledWith('w-1');
    expect(closeFailed).toHaveBeenCalledTimes(1);
    expect(closeFailed).toHaveBeenCalledWith('w-1');
  });

  it('says nothing when the user declines the confirmation', async () => {
    const closeRequested = vi.fn<(windowId: string) => void>();
    const closeFailed = vi.fn<(windowId: string) => void>();
    const { actions, ipc } = buildActions({
      confirmPort: vi.fn<ConfirmPort>().mockResolvedValue(false),
      closeRequested,
      closeFailed,
    });

    await actions.close(makeEntry({ status: 'running' }));

    expect(ipc.invokeCloseWindow).not.toHaveBeenCalled();
    expect(closeRequested).not.toHaveBeenCalled();
    expect(closeFailed).not.toHaveBeenCalled();
  });

  it('says nothing when the close is not allowed', async () => {
    vi.mocked(confirmTargetModule.resolveConfirmTarget).mockReturnValueOnce({
      allowed: false,
      requiresConfirmation: false,
      reason: 'unknown',
    });
    const closeRequested = vi.fn<(windowId: string) => void>();
    const closeFailed = vi.fn<(windowId: string) => void>();
    const { actions, ipc } = buildActions({
      confirmPort: approvingConfirmPort(),
      closeRequested,
      closeFailed,
    });

    await actions.close(makeEntry({ status: 'running' }));

    expect(ipc.invokeCloseWindow).not.toHaveBeenCalled();
    expect(closeRequested).not.toHaveBeenCalled();
    expect(closeFailed).not.toHaveBeenCalled();
  });
});
