/**
 * IPC contract for the `list_restore_candidates` and `update_window_layout`
 * commands. Their names follow the verb + object, snake_case convention and
 * mirror their protocol variants one for one. `@tauri-apps/api/core` is
 * mocked, so every assertion is about the calls this module makes.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

import { invokeListRestoreCandidates, invokeUpdateWindowLayout } from './session-client';
import { BoundaryError } from './session-boundary';

beforeEach(() => {
  invokeMock.mockReset();
});

describe('invokeListRestoreCandidates', () => {
  it('invokes list_restore_candidates with no arguments', async () => {
    invokeMock.mockResolvedValueOnce({ windows: [], taken_at_epoch_s: null });

    await invokeListRestoreCandidates();

    expect(invokeMock).toHaveBeenCalledWith('list_restore_candidates');
  });

  it('returns an empty candidate list and takenAtEpochS: null', async () => {
    invokeMock.mockResolvedValueOnce({ windows: [], taken_at_epoch_s: null });

    const result = await invokeListRestoreCandidates();

    expect(result).toEqual({ candidates: [], takenAtEpochS: null });
  });

  it('returns takenAtEpochS as the integer the daemon reported', async () => {
    invokeMock.mockResolvedValueOnce({ windows: [], taken_at_epoch_s: 1_735_000_000 });

    const result = await invokeListRestoreCandidates();

    expect(result.takenAtEpochS).toBe(1_735_000_000);
  });

  it('narrows each candidate through the same choke point as the boundary module', async () => {
    invokeMock.mockResolvedValueOnce({
      windows: [
        {
          position: 0,
          command: ['claude'],
          cwd: '/x',
          name: 'main',
          vendor: null,
          detection: null,
        },
      ],
      taken_at_epoch_s: null,
    });

    const result = await invokeListRestoreCandidates();

    expect(result.candidates).toHaveLength(1);
    expect(result.candidates[0]).toMatchObject({
      position: 0,
      command: ['claude'],
      cwd: '/x',
      name: 'main',
      layout: null,
    });
  });

  it('rejects with BoundaryError when windows is not an array', async () => {
    invokeMock.mockResolvedValueOnce({ windows: 'nope', taken_at_epoch_s: null });

    await expect(invokeListRestoreCandidates()).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when the reply is not an object', async () => {
    invokeMock.mockResolvedValueOnce('nope');

    await expect(invokeListRestoreCandidates()).rejects.toThrow(BoundaryError);
  });
});

describe('invokeUpdateWindowLayout', () => {
  const WINDOW_ID = 'w1-k3f9xa';
  const LAYOUT_JSON = '{"v":1,"pos":null,"size":null,"z":0,"min":false,"max":false,"tab":0}';

  it('invokes update_window_layout with window_id and layout_json argument keys, verbatim', async () => {
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID });

    await invokeUpdateWindowLayout(WINDOW_ID, LAYOUT_JSON);

    expect(invokeMock).toHaveBeenCalledWith('update_window_layout', {
      window_id: WINDOW_ID,
      layout_json: LAYOUT_JSON,
    });
  });

  it('narrows the reply to { windowId }', async () => {
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID });

    const result = await invokeUpdateWindowLayout(WINDOW_ID, LAYOUT_JSON);

    expect(result).toEqual({ windowId: WINDOW_ID });
  });

  it('rejects with BoundaryError when window_id is missing from the reply', async () => {
    invokeMock.mockResolvedValueOnce({});

    await expect(invokeUpdateWindowLayout(WINDOW_ID, LAYOUT_JSON)).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when the reply is not an object', async () => {
    invokeMock.mockResolvedValueOnce(WINDOW_ID);

    await expect(invokeUpdateWindowLayout(WINDOW_ID, LAYOUT_JSON)).rejects.toThrow(BoundaryError);
  });
});
