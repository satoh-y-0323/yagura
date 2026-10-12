/**
 * The attach command at the core boundary: attach names the size the window
 * will be drawn at; the window's screen follows as frames through the pull loop.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const { invokeMock, listenMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listenMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: listenMock,
}));

import { BoundaryError } from './session-boundary';
import { invokeAttachWindow } from './session-client';

type ListenCallback = (event: { payload: unknown }) => void;

const WINDOW_ID = 'w1-k3f9xa';

const registeredHandlers = new Map<string, ListenCallback>();

beforeEach(() => {
  invokeMock.mockReset();
  listenMock.mockReset();
  registeredHandlers.clear();
  listenMock.mockImplementation((name: string, callback: ListenCallback) => {
    registeredHandlers.set(name, callback);
    return Promise.resolve(vi.fn());
  });
});

describe('invokeAttachWindow', () => {
  it('sends the size the window will be drawn at', async () => {
    invokeMock.mockResolvedValueOnce({ child_exited: false, exit_code: null });

    const result = await invokeAttachWindow(WINDOW_ID, { cols: 120, rows: 40 });

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith('attach_window', {
      window_id: WINDOW_ID,
      size: { cols: 120, rows: 40 },
    });
    expect(result).toEqual({ child_exited: false, exit_code: null });
  });

  it('sends no size when there is none to send', async () => {
    invokeMock.mockResolvedValueOnce({ child_exited: false, exit_code: null });

    await invokeAttachWindow(WINDOW_ID, null);

    const args = invokeMock.mock.calls[0]?.[1] as Record<string, unknown>;
    expect(args.window_id).toBe(WINDOW_ID);
    expect(args.size ?? null).toBeNull();
  });

  it('rejects a result that does not say whether the child exited', async () => {
    invokeMock.mockResolvedValueOnce({ exit_code: null });

    await expect(invokeAttachWindow(WINDOW_ID, null)).rejects.toBeInstanceOf(BoundaryError);
  });
});
