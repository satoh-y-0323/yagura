/**
 * Contract for `invokeOpenWindow`'s optional fields and for the keymap
 * commands: `invokeLoadKeymap` sends no arguments and narrows a
 * `{ keys, legacy_entries, failure? }` reply without ever throwing on a load
 * failure; `invokeSaveKeymap` sends `{ keys }` and turns a rejection into a
 * result value. `@tauri-apps/api/core` is mocked, so every assertion is about
 * the calls this module makes.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

import { invokeOpenWindow, invokeLoadKeymap, invokeSaveKeymap } from './session-client';
import { BoundaryError } from './session-boundary';

const COMMAND = ['claude'];
const WINDOW_ID = 'w1-k3f9xa';

beforeEach(() => {
  invokeMock.mockReset();
});

describe('invokeOpenWindow with name and detection definition name', () => {
  it('sends both name and detection, never null, as part of the open_window call when provided', async () => {
    // The reply carries detection_warning alongside window_id; this suite
    // otherwise verifies argument shape, not the reply.
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID, detection_warning: null });

    const opened = await invokeOpenWindow({
      command: COMMAND,
      name: 'my-window',
      detection: 'claude-code',
    });

    expect(invokeMock).toHaveBeenCalledWith('open_window', {
      command: COMMAND,
      cwd: undefined,
      name: 'my-window',
      detection: 'claude-code',
    });
    const [, args] = invokeMock.mock.calls[0] as [string, Record<string, unknown>];
    expect(args['name']).not.toBeNull();
    expect(args['detection']).not.toBeNull();
    expect(opened).toEqual({ windowId: WINDOW_ID, detectionWarning: null });
  });

  it('passes provided name and detection through unchanged, without trimming or transforming', async () => {
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID, detection_warning: null });

    await invokeOpenWindow({
      command: COMMAND,
      name: '  My Window  ',
      detection: 'Claude-Code_v2',
    });

    expect(invokeMock).toHaveBeenCalledWith('open_window', {
      command: COMMAND,
      cwd: undefined,
      name: '  My Window  ',
      detection: 'Claude-Code_v2',
    });
  });

  it.each([
    { label: 'empty name, non-empty detection', name: '', detection: 'claude-code' },
    { label: 'non-empty name, empty detection', name: 'my-window', detection: '' },
  ])(
    'sends an empty-string field as not provided while still sending the non-empty one ($label)',
    async ({ name, detection }) => {
      invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID, detection_warning: null });

      await invokeOpenWindow({ command: COMMAND, name, detection });

      const expectedArgs: Record<string, unknown> = { command: COMMAND, cwd: undefined };
      if (name !== '') {
        expectedArgs['name'] = name;
      }
      if (detection !== '') {
        expectedArgs['detection'] = detection;
      }
      expect(invokeMock).toHaveBeenCalledWith('open_window', expectedArgs);
    }
  );

  it('propagates a daemon rejection identifiably when the call includes name and detection', async () => {
    invokeMock.mockRejectedValueOnce(new Error('window name already in use'));

    await expect(
      invokeOpenWindow({ command: COMMAND, name: 'duplicate-name', detection: 'claude-code' })
    ).rejects.toThrow('window name already in use');
    expect(invokeMock).toHaveBeenCalledWith('open_window', {
      command: COMMAND,
      cwd: undefined,
      name: 'duplicate-name',
      detection: 'claude-code',
    });
  });
});

describe('invokeLoadKeymap', () => {
  it('calls load_keymap with no arguments', async () => {
    invokeMock.mockResolvedValueOnce({ keys: {}, legacy_entries: 0 });

    await invokeLoadKeymap();

    expect(invokeMock).toHaveBeenCalledWith('load_keymap');
  });

  it('returns the keys table and the legacy entry count with no failure when the file loads', async () => {
    const rawKeys = {
      'focus-next-window': 'Ctrl+Alt+ArrowRight',
      'toggle-chat': '',
    };
    invokeMock.mockResolvedValueOnce({ keys: rawKeys, legacy_entries: 3 });

    const result = await invokeLoadKeymap();

    expect(result).toEqual({ keys: rawKeys, legacy_entries: 3 });
    expect(Object.prototype.hasOwnProperty.call(result, 'failure')).toBe(false);
  });

  it.each([['not_found'], ['read_failed'], ['invalid_toml'], ['too_large']] as const)(
    'returns failure "%s" with an empty table instead of throwing',
    async (failure) => {
      invokeMock.mockResolvedValueOnce({ keys: {}, legacy_entries: 0, failure });

      const result = await invokeLoadKeymap();

      expect(result).toEqual({ keys: {}, legacy_entries: 0, failure });
    }
  );

  it('resolves rather than rejects when the daemon reports a load failure', async () => {
    invokeMock.mockResolvedValueOnce({ keys: {}, legacy_entries: 0, failure: 'invalid_toml' });

    await expect(invokeLoadKeymap()).resolves.toBeDefined();
  });

  it('rejects with BoundaryError when the reply is an array', async () => {
    invokeMock.mockResolvedValueOnce([]);

    await expect(invokeLoadKeymap()).rejects.toThrow(BoundaryError);
  });

  it.each([
    ['a number', 7],
    ['null', null],
    ['an object', { chord: 'x' }],
  ])('rejects with BoundaryError when a keys value is %s', async (_label, value) => {
    invokeMock.mockResolvedValueOnce({ keys: { 'toggle-chat': value }, legacy_entries: 0 });

    await expect(invokeLoadKeymap()).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when keys is missing or not a record', async () => {
    invokeMock.mockResolvedValueOnce({ legacy_entries: 0 });
    await expect(invokeLoadKeymap()).rejects.toThrow(BoundaryError);

    invokeMock.mockResolvedValueOnce({ keys: [], legacy_entries: 0 });
    await expect(invokeLoadKeymap()).rejects.toThrow(BoundaryError);
  });

  it.each([
    ['missing', undefined],
    ['a string', '3'],
    ['null', null],
    ['negative', -1],
    ['fractional', 1.5],
    ['NaN', Number.NaN],
    ['infinite', Number.POSITIVE_INFINITY],
  ])('rejects with BoundaryError when legacy_entries is %s', async (_label, value) => {
    const reply: Record<string, unknown> = { keys: {} };
    if (value !== undefined) reply['legacy_entries'] = value;
    invokeMock.mockResolvedValueOnce(reply);

    await expect(invokeLoadKeymap()).rejects.toThrow(BoundaryError);
  });

  it('accepts zero as a legacy entry count, as the boundary of the non-negative rule', async () => {
    invokeMock.mockResolvedValueOnce({ keys: {}, legacy_entries: 0 });

    await expect(invokeLoadKeymap()).resolves.toEqual({ keys: {}, legacy_entries: 0 });
  });

  it.each([['unknown_reason'], [''], [3], [null]])(
    'rejects with BoundaryError for the unrecognized failure %j',
    async (failure) => {
      invokeMock.mockResolvedValueOnce({ keys: {}, legacy_entries: 0, failure });

      await expect(invokeLoadKeymap()).rejects.toThrow(BoundaryError);
    }
  );

  it('does not read keys or legacy_entries that only the prototype carries', async () => {
    const reply = Object.create({ keys: {}, legacy_entries: 0 }) as Record<string, unknown>;
    invokeMock.mockResolvedValueOnce(reply);

    await expect(invokeLoadKeymap()).rejects.toThrow(BoundaryError);
  });
});

describe('invokeSaveKeymap', () => {
  it('calls save_keymap with the table under the keys argument', async () => {
    invokeMock.mockResolvedValueOnce(null);
    const keys = { 'toggle-chat': 'Ctrl+Alt+c', 'close-window': '' };

    await invokeSaveKeymap(keys);

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith('save_keymap', { keys });
  });

  it('reports success when the command resolves', async () => {
    invokeMock.mockResolvedValueOnce(null);

    const result = await invokeSaveKeymap({});

    expect(result.ok).toBe(true);
  });

  it.each([['no_config_root'], ['too_large'], ['write_failed']] as const)(
    'turns the rejection "%s" into a failed result instead of throwing',
    async (reason) => {
      invokeMock.mockRejectedValueOnce(reason);

      const result = await invokeSaveKeymap({ 'toggle-chat': 'Ctrl+Alt+c' });

      expect(result).toEqual({ ok: false, reason });
    }
  );

  it.each([
    ['an unknown string', 'disk_on_fire'],
    ['an empty string', ''],
    ['an Error', new Error('boom')],
    ['an object', { reason: 'too_large' }],
  ])('rejects with BoundaryError for %s rejection value', async (_label, rejection) => {
    invokeMock.mockRejectedValueOnce(rejection);

    await expect(invokeSaveKeymap({})).rejects.toThrow(BoundaryError);
  });
});
