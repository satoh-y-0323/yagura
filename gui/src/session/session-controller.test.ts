/**
 * Regression lock for the isSessionInteractive guard: no input reaches the core
 * while the session is not interactive.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';

vi.mock('../ipc/session-client', () => ({
  invokeSendWindowInput: vi.fn().mockResolvedValue(undefined),
  invokeClearWindowSelection: vi.fn().mockResolvedValue(undefined),
}));

import { invokeClearWindowSelection, invokeSendWindowInput } from '../ipc/session-client';
import { SessionController } from './session-controller';
import { createWindowSessionStore } from '../stores/session-store';

const WINDOW_ID = 'w1-k3f9xa';
const SHIFT_ENTER = {
  type: 'key',
  key: { named: 'enter' },
  mods: { shift: true, alt: false, ctrl: false },
} as const;

describe('SessionController.sendInput', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('does not forward input while the session is not interactive', () => {
    const store = createWindowSessionStore();
    store.markConnecting();
    const controller = new SessionController(WINDOW_ID, store);

    controller.sendInput(SHIFT_ENTER);

    expect(vi.mocked(invokeSendWindowInput)).toHaveBeenCalledTimes(0);
  });

  it('forwards the input with the window id when the session is interactive', () => {
    const store = createWindowSessionStore();
    store.markAttached();
    const controller = new SessionController(WINDOW_ID, store);

    controller.sendInput(SHIFT_ENTER);

    expect(vi.mocked(invokeSendWindowInput)).toHaveBeenCalledTimes(1);
    expect(vi.mocked(invokeSendWindowInput)).toHaveBeenCalledWith(WINDOW_ID, SHIFT_ENTER);
  });
});

describe('SessionController.clearSelection', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('asks the core to clear the window selection when the session is interactive', () => {
    const store = createWindowSessionStore();
    store.markAttached();
    const controller = new SessionController(WINDOW_ID, store);

    controller.clearSelection();

    expect(vi.mocked(invokeClearWindowSelection)).toHaveBeenCalledWith(WINDOW_ID);
  });

  it('does nothing while the session is not interactive', () => {
    const store = createWindowSessionStore();
    store.markConnecting();
    const controller = new SessionController(WINDOW_ID, store);

    controller.clearSelection();

    expect(vi.mocked(invokeClearWindowSelection)).toHaveBeenCalledTimes(0);
  });
});
