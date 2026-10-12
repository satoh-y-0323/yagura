/**
 * TS-boundary contract for `invokeCloseWindow`'s 3-value outcome.
 * The daemon-side round trip is covered by the Tauri core's
 * own tests; what is measured here is the command this module invokes, and
 * that every reply it hands on has been through `narrowCloseOutcome`.
 *
 * `not_found` and `unreachable` are both resolved values, never thrown: the
 * caller treats either the same as `closed` -- a signal to remove the pane.
 *
 * `@tauri-apps/api/core` is mocked, so every assertion is about the calls
 * this module makes. It lives in its own file, like
 * `session-client.sync-approvals.test.ts`, so the outcome shapes below are
 * read in one place.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

import { BoundaryError } from './session-boundary';
import { invokeCloseWindow } from './session-client';

/** Sample window id, matching `session-client.test.ts`'s `WINDOW_A`. */
const WINDOW_A = 'w1-k3f9xa';

beforeEach(() => {
  invokeMock.mockReset();
});

describe('invokeCloseWindow: the command it calls', () => {
  it('calls close_window with the window id and no force flag', async () => {
    invokeMock.mockResolvedValueOnce('closed');

    await invokeCloseWindow(WINDOW_A);

    expect(invokeMock).toHaveBeenCalledWith('close_window', {
      window_id: WINDOW_A,
      force: undefined,
    });
  });
});

describe('invokeCloseWindow: the outcomes it resolves', () => {
  it('resolves "not_found"', async () => {
    invokeMock.mockResolvedValueOnce('not_found');

    await expect(invokeCloseWindow(WINDOW_A)).resolves.toBe('not_found');
  });

  it('resolves "unreachable"', async () => {
    invokeMock.mockResolvedValueOnce('unreachable');

    await expect(invokeCloseWindow(WINDOW_A)).resolves.toBe('unreachable');
  });

  it('resolves "closed"', async () => {
    invokeMock.mockResolvedValueOnce('closed');

    await expect(invokeCloseWindow(WINDOW_A)).resolves.toBe('closed');
  });
});

describe('invokeCloseWindow: replies that violate the contract', () => {
  it('rejects an undefined reply', async () => {
    invokeMock.mockResolvedValueOnce(undefined);

    await expect(invokeCloseWindow(WINDOW_A)).rejects.toThrow(BoundaryError);
  });

  it('rejects a reply shaped like a status wrapper', async () => {
    invokeMock.mockResolvedValueOnce({ status: 'closed' });

    await expect(invokeCloseWindow(WINDOW_A)).rejects.toThrow(BoundaryError);
  });
});
