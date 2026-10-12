/**
 * `window-actions.ts` is the single execution path for close / interrupt / rename /
 * focus / minimize / maximize; move and resize stay out because they are continuous,
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
    vendor: null,
    cwd: 'C:\\Users\\dev\\project',
    detected: status === 'unknown' ? { status, reason: null } : { status },
    statusRev: null,
    exitCode: null,
    room: null,
    detectionWarning: null,
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

/**
 * The tab-mode deps `focus` and `minimize` reuse. All three are optional and
 * fail-safe: without `isTabMode` or `switchToTab` both operations keep their
 * non-tab behaviour, and `resolveTabSuccessor` is consulted only in tab mode.
 */
type MockSwitchToTab = Mock<(windowId: string) => void>;
type MockIsTabMode = Mock<() => boolean>;
type MockResolveTabSuccessor = Mock<(windowId: string) => string | null>;

function buildActions(
  overrides: {
    registry?: MockRegistry;
    ipc?: MockIpc;
    confirmPort?: ConfirmPort;
    removeWindow?: (windowId: string) => void;
    isTabMode?: () => boolean;
    switchToTab?: (windowId: string) => void;
    resolveTabSuccessor?: (windowId: string) => string | null;
  } = {}
): {
  actions: WindowActions;
  registry: MockRegistry;
  ipc: MockIpc;
} {
  const registry = overrides.registry ?? makeRegistry();
  const ipc = overrides.ipc ?? makeIpc();
  const actions = createWindowActions({
    registry,
    ipc,
    // exactOptionalPropertyTypes: passing `confirmPort: undefined` is a different
    // (rejected) shape from omitting the key entirely.
    ...(overrides.confirmPort !== undefined ? { confirmPort: overrides.confirmPort } : {}),
    ...(overrides.removeWindow !== undefined ? { removeWindow: overrides.removeWindow } : {}),
    ...(overrides.isTabMode !== undefined ? { isTabMode: overrides.isTabMode } : {}),
    ...(overrides.switchToTab !== undefined ? { switchToTab: overrides.switchToTab } : {}),
    ...(overrides.resolveTabSuccessor !== undefined
      ? { resolveTabSuccessor: overrides.resolveTabSuccessor }
      : {}),
  });
  return { actions, registry, ipc };
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe('window-actions: close honors resolveConfirmTarget instead of re-implementing it', () => {
  it('allows close for a window in unknown status (close stays available)', async () => {
    const confirmPort = approvingConfirmPort();
    const { actions, ipc } = buildActions({ confirmPort });

    await actions.close(makeEntry({ status: 'unknown' }));

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

    await actions.interrupt(makeEntry({ status: 'running' }));

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

describe('window-actions: focus brings the window to front and restores it if minimized', () => {
  it('brings a normal window to front by delegating to the registry, not recomputing zOrder', () => {
    const { actions, registry } = buildActions();

    actions.focus(makeEntry({ status: 'running' }));

    expect(registry.bringWindowToFront).toHaveBeenCalledTimes(1);
    expect(registry.bringWindowToFront).toHaveBeenCalledWith('w-1');
  });

  it('does not touch the minimized flag for a window that is not minimized', () => {
    const { actions, registry } = buildActions();

    actions.focus(makeEntry({ display: { ...makeEntry().display, isMinimized: false } }));

    expect(registry.setWindowMinimized).not.toHaveBeenCalled();
  });

  it('restores a minimized window when it is focused', () => {
    const { actions, registry } = buildActions();

    actions.focus(makeEntry({ display: { ...makeEntry().display, isMinimized: true } }));

    expect(registry.setWindowMinimized).toHaveBeenCalledWith('w-1', false);
  });

  it('still brings a minimized window to front, not just restoring it', () => {
    const { actions, registry } = buildActions();

    actions.focus(makeEntry({ display: { ...makeEntry().display, isMinimized: true } }));

    expect(registry.bringWindowToFront).toHaveBeenCalledWith('w-1');
  });
});

describe('window-actions: minimize and maximize delegate to the registry without touching zOrder', () => {
  it('minimizes a window through the registry mutator', () => {
    const { actions, registry } = buildActions();

    actions.minimize(makeEntry());

    expect(registry.setWindowMinimized).toHaveBeenCalledWith('w-1', true);
  });

  it('does not re-stack the window when minimizing it', () => {
    const { actions, registry } = buildActions();

    actions.minimize(makeEntry());

    expect(registry.bringWindowToFront).not.toHaveBeenCalled();
  });

  it('toggles maximize on for a window that is not maximized', () => {
    const { actions, registry } = buildActions();

    actions.maximize(makeEntry({ display: { ...makeEntry().display, isMaximized: false } }));

    expect(registry.setWindowMaximized).toHaveBeenCalledWith('w-1', true);
  });

  it('toggles maximize off for a window that is already maximized', () => {
    const { actions, registry } = buildActions();

    actions.maximize(makeEntry({ display: { ...makeEntry().display, isMaximized: true } }));

    expect(registry.setWindowMaximized).toHaveBeenCalledWith('w-1', false);
  });

  it('does not re-stack the window when maximizing it', () => {
    const { actions, registry } = buildActions();

    actions.maximize(makeEntry());

    expect(registry.bringWindowToFront).not.toHaveBeenCalled();
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

describe('window-actions: focus switches tabs while in tab mode, instead of raising a float window', () => {
  it('calls switchToTab(windowId) instead of bringWindowToFront when isTabMode() is true', () => {
    const isTabMode: MockIsTabMode = vi.fn<() => boolean>().mockReturnValue(true);
    const switchToTab: MockSwitchToTab = vi.fn<(windowId: string) => void>();
    const { actions, registry } = buildActions({ isTabMode, switchToTab });

    actions.focus(makeEntry({ status: 'running' }));

    expect(switchToTab).toHaveBeenCalledWith('w-1');
    expect(registry.bringWindowToFront).not.toHaveBeenCalled();
  });

  // `switchToTab` (`App.svelte`) already unminimizes, so focus's own guard must not
  // also fire when it delegates — setWindowMinimized would be dispatched twice.
  it('delegates directly to switchToTab for a minimized window in tab mode, without calling its own setWindowMinimized guard first (switchToTab owns the unminimize)', () => {
    const isTabMode: MockIsTabMode = vi.fn<() => boolean>().mockReturnValue(true);
    const switchToTab: MockSwitchToTab = vi.fn<(windowId: string) => void>();
    const { actions, registry } = buildActions({ isTabMode, switchToTab });

    actions.focus(makeEntry({ display: { ...makeEntry().display, isMinimized: true } }));

    expect(switchToTab).toHaveBeenCalledWith('w-1');
    expect(registry.setWindowMinimized).not.toHaveBeenCalled();
  });

  it('does not touch setWindowMinimized for a window that is not minimized, in tab mode', () => {
    const isTabMode: MockIsTabMode = vi.fn<() => boolean>().mockReturnValue(true);
    const switchToTab: MockSwitchToTab = vi.fn<(windowId: string) => void>();
    const { actions, registry } = buildActions({ isTabMode, switchToTab });

    actions.focus(makeEntry({ display: { ...makeEntry().display, isMinimized: false } }));

    expect(registry.setWindowMinimized).not.toHaveBeenCalled();
  });

  it('keeps the existing behaviour when isTabMode() returns false, even with switchToTab supplied', () => {
    const isTabMode: MockIsTabMode = vi.fn<() => boolean>().mockReturnValue(false);
    const switchToTab: MockSwitchToTab = vi.fn<(windowId: string) => void>();
    const { actions, registry } = buildActions({ isTabMode, switchToTab });

    actions.focus(makeEntry({ status: 'running' }));

    expect(switchToTab).not.toHaveBeenCalled();
    expect(registry.bringWindowToFront).toHaveBeenCalledWith('w-1');
  });

  it('keeps the existing behaviour when isTabMode/switchToTab are not supplied at all (fail-safe)', () => {
    const { actions, registry } = buildActions();

    actions.focus(makeEntry({ status: 'running' }));

    expect(registry.bringWindowToFront).toHaveBeenCalledWith('w-1');
  });
});

describe('window-actions: minimize switches to the tab successor while in tab mode, or ends tab mode', () => {
  it('always minimizes the window through the registry mutator first, tab mode or not', () => {
    const isTabMode: MockIsTabMode = vi.fn<() => boolean>().mockReturnValue(true);
    const switchToTab: MockSwitchToTab = vi.fn<(windowId: string) => void>();
    const resolveTabSuccessor: MockResolveTabSuccessor = vi
      .fn<(windowId: string) => string | null>()
      .mockReturnValue('w-2');
    const { actions, registry } = buildActions({ isTabMode, switchToTab, resolveTabSuccessor });

    actions.minimize(makeEntry());

    expect(registry.setWindowMinimized).toHaveBeenCalledWith('w-1', true);
  });

  // The tab-successor branch is only meaningful for the active (maximized) tab.
  it('switches to the resolved successor when one remains, in tab mode', () => {
    const isTabMode: MockIsTabMode = vi.fn<() => boolean>().mockReturnValue(true);
    const switchToTab: MockSwitchToTab = vi.fn<(windowId: string) => void>();
    const resolveTabSuccessor: MockResolveTabSuccessor = vi
      .fn<(windowId: string) => string | null>()
      .mockReturnValue('w-2');
    const { actions, registry } = buildActions({ isTabMode, switchToTab, resolveTabSuccessor });

    actions.minimize(makeEntry({ display: { ...makeEntry().display, isMaximized: true } }));

    expect(resolveTabSuccessor).toHaveBeenCalledWith('w-1');
    expect(switchToTab).toHaveBeenCalledWith('w-2');
    expect(registry.setWindowMaximized).not.toHaveBeenCalled();
  });

  it('ends tab mode (setWindowMaximized(id, false)) when no successor remains', () => {
    const isTabMode: MockIsTabMode = vi.fn<() => boolean>().mockReturnValue(true);
    const switchToTab: MockSwitchToTab = vi.fn<(windowId: string) => void>();
    const resolveTabSuccessor: MockResolveTabSuccessor = vi
      .fn<(windowId: string) => string | null>()
      .mockReturnValue(null);
    const { actions, registry } = buildActions({ isTabMode, switchToTab, resolveTabSuccessor });

    actions.minimize(makeEntry({ display: { ...makeEntry().display, isMaximized: true } }));

    expect(switchToTab).not.toHaveBeenCalled();
    expect(registry.setWindowMaximized).toHaveBeenCalledWith('w-1', false);
  });

  // entry.display.isMaximized gates the branch here, rather than relying on the UI
  // wiring to call actions.minimize only for the active tab.
  it('does not consult resolveTabSuccessor when entry.display.isMaximized is false, even in tab mode', () => {
    const isTabMode: MockIsTabMode = vi.fn<() => boolean>().mockReturnValue(true);
    const switchToTab: MockSwitchToTab = vi.fn<(windowId: string) => void>();
    const resolveTabSuccessor: MockResolveTabSuccessor = vi
      .fn<(windowId: string) => string | null>()
      .mockReturnValue('w-2');
    const { actions, registry } = buildActions({ isTabMode, switchToTab, resolveTabSuccessor });

    actions.minimize(makeEntry({ display: { ...makeEntry().display, isMaximized: false } }));

    expect(resolveTabSuccessor).not.toHaveBeenCalled();
    expect(switchToTab).not.toHaveBeenCalled();
    expect(registry.setWindowMaximized).not.toHaveBeenCalled();
  });

  it('still minimizes the window through the registry mutator even when the tab-successor branch is skipped (isMaximized: false)', () => {
    const isTabMode: MockIsTabMode = vi.fn<() => boolean>().mockReturnValue(true);
    const switchToTab: MockSwitchToTab = vi.fn<(windowId: string) => void>();
    const resolveTabSuccessor: MockResolveTabSuccessor = vi
      .fn<(windowId: string) => string | null>()
      .mockReturnValue('w-2');
    const { actions, registry } = buildActions({ isTabMode, switchToTab, resolveTabSuccessor });

    actions.minimize(makeEntry({ display: { ...makeEntry().display, isMaximized: false } }));

    expect(registry.setWindowMinimized).toHaveBeenCalledWith('w-1', true);
  });

  it('does not consult resolveTabSuccessor outside tab mode', () => {
    const isTabMode: MockIsTabMode = vi.fn<() => boolean>().mockReturnValue(false);
    const resolveTabSuccessor: MockResolveTabSuccessor =
      vi.fn<(windowId: string) => string | null>();
    const { actions, registry } = buildActions({ isTabMode, resolveTabSuccessor });

    actions.minimize(makeEntry());

    expect(resolveTabSuccessor).not.toHaveBeenCalled();
    expect(registry.setWindowMaximized).not.toHaveBeenCalled();
  });

  it('keeps the existing behaviour (minimize only) when the tab-mode deps are not supplied at all (fail-safe)', () => {
    const { actions, registry } = buildActions();

    actions.minimize(makeEntry());

    expect(registry.setWindowMinimized).toHaveBeenCalledWith('w-1', true);
    expect(registry.setWindowMaximized).not.toHaveBeenCalled();
    expect(registry.bringWindowToFront).not.toHaveBeenCalled();
  });

  it('does not re-stack the window (bringWindowToFront) when minimizing it, even in tab mode', () => {
    const isTabMode: MockIsTabMode = vi.fn<() => boolean>().mockReturnValue(true);
    const switchToTab: MockSwitchToTab = vi.fn<(windowId: string) => void>();
    const resolveTabSuccessor: MockResolveTabSuccessor = vi
      .fn<(windowId: string) => string | null>()
      .mockReturnValue('w-2');
    const { actions, registry } = buildActions({ isTabMode, switchToTab, resolveTabSuccessor });

    actions.minimize(makeEntry());

    expect(registry.bringWindowToFront).not.toHaveBeenCalled();
  });
});

describe('window-actions: tab mode needs every dep it uses', () => {
  const maximized = (): WindowEntry =>
    makeEntry({ display: { ...makeEntry().display, isMaximized: true } });

  it('raises the float window when switchToTab is supplied but isTabMode is not', () => {
    const switchToTab: MockSwitchToTab = vi.fn<(windowId: string) => void>();
    const { actions, registry } = buildActions({ switchToTab });

    actions.focus(makeEntry());

    expect(switchToTab).not.toHaveBeenCalled();
    expect(registry.bringWindowToFront).toHaveBeenCalledWith('w-1');
  });

  it('raises the float window when isTabMode says yes but there is no switchToTab', () => {
    const isTabMode: MockIsTabMode = vi.fn<() => boolean>().mockReturnValue(true);
    const { actions, registry } = buildActions({ isTabMode });

    actions.focus(makeEntry());

    expect(registry.bringWindowToFront).toHaveBeenCalledWith('w-1');
  });

  it('only minimizes a maximized window when isTabMode is not supplied', () => {
    const switchToTab: MockSwitchToTab = vi.fn<(windowId: string) => void>();
    const resolveTabSuccessor: MockResolveTabSuccessor = vi
      .fn<(windowId: string) => string | null>()
      .mockReturnValue('w-2');
    const { actions, registry } = buildActions({ switchToTab, resolveTabSuccessor });

    actions.minimize(maximized());

    expect(registry.setWindowMinimized).toHaveBeenCalledWith('w-1', true);
    expect(resolveTabSuccessor).not.toHaveBeenCalled();
    expect(switchToTab).not.toHaveBeenCalled();
  });

  it('only minimizes a maximized window in tab mode when there is no resolveTabSuccessor', () => {
    const isTabMode: MockIsTabMode = vi.fn<() => boolean>().mockReturnValue(true);
    const switchToTab: MockSwitchToTab = vi.fn<(windowId: string) => void>();
    const { actions, registry } = buildActions({ isTabMode, switchToTab });

    actions.minimize(maximized());

    expect(registry.setWindowMinimized).toHaveBeenCalledWith('w-1', true);
    expect(switchToTab).not.toHaveBeenCalled();
    expect(registry.setWindowMaximized).not.toHaveBeenCalled();
  });

  it('only minimizes a maximized window in tab mode when there is no switchToTab', () => {
    const isTabMode: MockIsTabMode = vi.fn<() => boolean>().mockReturnValue(true);
    const resolveTabSuccessor: MockResolveTabSuccessor = vi
      .fn<(windowId: string) => string | null>()
      .mockReturnValue(null);
    const { actions, registry } = buildActions({ isTabMode, resolveTabSuccessor });

    actions.minimize(maximized());

    expect(resolveTabSuccessor).not.toHaveBeenCalled();
    expect(registry.setWindowMaximized).not.toHaveBeenCalled();
  });
});
