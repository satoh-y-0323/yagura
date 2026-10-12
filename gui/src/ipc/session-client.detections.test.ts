/**
 * IPC contract for the detection-definition list command and the `open_window`
 * reply's warning field. Both warning fields accept the same four wire forms:
 * key missing and JSON null narrow to null, a string passes through,
 * anything else raises BoundaryError. `@tauri-apps/api/core` is mocked, so
 * every assertion is about the calls this module makes.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

import { invokeListDetections, invokeOpenWindow } from './session-client';
import { BoundaryError } from './session-boundary';

const COMMAND = ['claude'];
const WINDOW_ID = 'w1-k3f9xa';

beforeEach(() => {
  invokeMock.mockReset();
});

describe('invokeListDetections', () => {
  it('invokes list_detections with no arguments', async () => {
    invokeMock.mockResolvedValueOnce({ names: [] });

    await invokeListDetections();

    expect(invokeMock).toHaveBeenCalledWith('list_detections');
  });

  it('returns the names array and warning: null when warning is absent from the reply', async () => {
    invokeMock.mockResolvedValueOnce({ names: ['claude-code', 'generic-shell'] });

    const result = await invokeListDetections();

    expect(result).toEqual({ names: ['claude-code', 'generic-shell'], warning: null });
  });

  it('returns an empty names array unchanged', async () => {
    invokeMock.mockResolvedValueOnce({ names: [] });

    const result = await invokeListDetections();

    expect(result.names).toEqual([]);
  });

  it('rejects with BoundaryError when names is not an array', async () => {
    invokeMock.mockResolvedValueOnce({ names: 'claude-code' });

    await expect(invokeListDetections()).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when a names entry is not a string', async () => {
    invokeMock.mockResolvedValueOnce({ names: ['claude-code', 42] });

    await expect(invokeListDetections()).rejects.toThrow(BoundaryError);
  });

  describe.each([
    { label: 'key missing', raw: {}, expected: null },
    { label: 'JSON null', raw: { warning: null }, expected: null },
    {
      label: 'string',
      raw: { warning: 'failed to enumerate detection definitions (os error 3)' },
      expected: 'failed to enumerate detection definitions (os error 3)',
    },
  ])('warning: $label', ({ raw, expected }) => {
    it('narrows to the expected value', async () => {
      invokeMock.mockResolvedValueOnce({ names: [], ...raw });

      const result = await invokeListDetections();

      expect(result.warning).toBe(expected);
    });
  });

  it.each([
    ['number', 42],
    ['boolean', true],
    ['object', { code: 'EIO' }],
    ['array', ['x']],
  ])(
    'rejects with BoundaryError when warning is a %s, not a string or null',
    async (_label, badWarning) => {
      invokeMock.mockResolvedValueOnce({ names: [], warning: badWarning });

      await expect(invokeListDetections()).rejects.toThrow(BoundaryError);
    }
  );
});

describe('invokeOpenWindow: reply shape', () => {
  it('resolves { windowId, detectionWarning: null } when the reply omits detection_warning', async () => {
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID });

    const result = await invokeOpenWindow({ command: COMMAND });

    expect(result).toEqual({ windowId: WINDOW_ID, detectionWarning: null });
  });

  it('resolves detectionWarning: null when the reply carries JSON null', async () => {
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID, detection_warning: null });

    const result = await invokeOpenWindow({ command: COMMAND });

    expect(result).toEqual({ windowId: WINDOW_ID, detectionWarning: null });
  });

  it('passes a string detection_warning through unchanged', async () => {
    const warning = 'detection definition file not found (os error 2); status stays unknown';
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID, detection_warning: warning });

    const result = await invokeOpenWindow({ command: COMMAND });

    expect(result).toEqual({ windowId: WINDOW_ID, detectionWarning: warning });
  });

  it.each([
    ['number', 42],
    ['boolean', false],
    ['object', { code: 'EIO' }],
  ])('rejects with BoundaryError when detection_warning is a %s', async (_label, badWarning) => {
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID, detection_warning: badWarning });

    await expect(invokeOpenWindow({ command: COMMAND })).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when the reply is a bare string', async () => {
    invokeMock.mockResolvedValueOnce(WINDOW_ID);

    await expect(invokeOpenWindow({ command: COMMAND })).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when window_id is missing', async () => {
    invokeMock.mockResolvedValueOnce({ detection_warning: null });

    await expect(invokeOpenWindow({ command: COMMAND })).rejects.toThrow(BoundaryError);
  });
});
