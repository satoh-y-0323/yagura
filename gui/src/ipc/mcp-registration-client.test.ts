/**
 * TS-boundary contract for the MCP registration bridge: the two Tauri
 * commands the WebView calls (`list_mcp_registrations`,
 * `run_mcp_registration`) and the subscriptions to the two pushes the core
 * forwards (`session://mcp-registration-status`,
 * `session://mcp-registration-result`).
 *
 * `@tauri-apps/api/core` and `@tauri-apps/api/event` are mocked, so every
 * assertion is about the calls this module makes and the shape it produces
 * from snake_case wire values, not about a real Tauri runtime.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const { invokeMock, listenMock, unlistenMock } = vi.hoisted(() => ({
  invokeMock: vi.fn<(command: string, args?: Record<string, unknown>) => Promise<unknown>>(),
  listenMock:
    vi.fn<(event: string, callback: (event: { payload: unknown }) => void) => Promise<unknown>>(),
  unlistenMock: vi.fn<() => void>(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: listenMock,
}));

import { BoundaryError, SESSION_EVENT } from './session-boundary';
import {
  invokeListMcpRegistrations,
  invokeRunMcpRegistration,
  listenMcpRegistrationResult,
  listenMcpRegistrationStatus,
} from './mcp-registration-client';
import type {
  McpRegistrationRequested,
  McpRegistrationResultPayload,
  McpRegistrationsListed,
  McpRegistrationStatusPayload,
} from './mcp-registration-boundary';

const STATUS_EVENT = 'session://mcp-registration-status';
const RESULT_EVENT = 'session://mcp-registration-result';

/** A Tauri event callback, as registered through the mocked `listen`. */
type ListenCallback = (event: { payload: unknown }) => void;

function commandWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    argv: ['alpha', 'mcp', 'add', 'yagura', '--', 'C:\\bridge\\yagura-mcp.exe'],
    program_at: 0,
    ...overrides,
  };
}

function rowWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    profile: 'alpha-cli',
    display_name: 'Alpha CLI',
    group: null,
    add: commandWire(),
    remove: commandWire({ argv: ['alpha', 'mcp', 'remove', 'yagura'] }),
    toml_set: {
      file: '.alpha/config.toml',
      table: ['mcp_servers', 'yagura'],
      values: [{ key: 'call_timeout', value: '86400' }],
    },
    ...overrides,
  };
}

function listedWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    request_id: 7,
    bridge: 'C:\\bridge\\yagura-mcp.exe',
    rows: [rowWire()],
    ...overrides,
  };
}

function statusInfoWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    profile: 'alpha-cli',
    state: 'differs',
    differs_in: ['command'],
    reason: null,
    add: commandWire(),
    remove: null,
    ...overrides,
  };
}

function statusPayloadWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return { request_id: 7, status: statusInfoWire(), ...overrides };
}

function stepWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    argv: ['alpha', 'mcp', 'add', 'yagura', '--', 'C:\\bridge\\yagura-mcp.exe'],
    program_at: 0,
    exit_code: 0,
    output: 'Added.',
    ...overrides,
  };
}

function resultPayloadWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    request_id: 8,
    profile: 'alpha-cli',
    action: 'register',
    steps: [stepWire(), stepWire({ argv: [], program_at: 0, exit_code: null, output: 'set' })],
    error: null,
    status: statusInfoWire({ state: 'registered', differs_in: [] }),
    ...overrides,
  };
}

beforeEach(() => {
  invokeMock.mockReset();
  listenMock.mockReset();
  unlistenMock.mockReset();
  listenMock.mockResolvedValue(unlistenMock);
});

describe('SESSION_EVENT for MCP registration pushes', () => {
  it('names the status push and the result push', () => {
    const names: Record<string, unknown> = SESSION_EVENT;

    expect(names['mcpRegistrationStatus']).toBe(STATUS_EVENT);
    expect(names['mcpRegistrationResult']).toBe(RESULT_EVENT);
  });
});

describe('invokeListMcpRegistrations', () => {
  it('calls list_mcp_registrations with the caller-chosen request id alone', async () => {
    invokeMock.mockResolvedValueOnce(listedWire());

    await invokeListMcpRegistrations(7);

    expect(invokeMock).toHaveBeenCalledWith('list_mcp_registrations', { request_id: 7 });
  });

  it('narrows the snake_case reply into camelCase rows', async () => {
    invokeMock.mockResolvedValueOnce(listedWire());

    const listed: McpRegistrationsListed = await invokeListMcpRegistrations(7);

    expect(listed).toEqual({
      requestId: 7,
      bridge: 'C:\\bridge\\yagura-mcp.exe',
      rows: [
        {
          profile: 'alpha-cli',
          displayName: 'Alpha CLI',
          group: null,
          add: {
            argv: ['alpha', 'mcp', 'add', 'yagura', '--', 'C:\\bridge\\yagura-mcp.exe'],
            programAt: 0,
          },
          remove: { argv: ['alpha', 'mcp', 'remove', 'yagura'], programAt: 0 },
          tomlSet: {
            file: '.alpha/config.toml',
            table: ['mcp_servers', 'yagura'],
            values: [{ key: 'call_timeout', value: '86400' }],
          },
          hookEntries: [],
        },
      ],
    });
  });

  it('accepts null for the bridge, a group and the settings-file change', async () => {
    invokeMock.mockResolvedValueOnce(
      listedWire({ bridge: null, rows: [rowWire({ group: 'Linux', toml_set: null })] })
    );

    const listed: McpRegistrationsListed = await invokeListMcpRegistrations(7);

    expect(listed.bridge).toBeNull();
    expect(listed.rows[0]?.group).toBe('Linux');
    expect(listed.rows[0]?.tomlSet).toBeNull();
  });

  it('rejects with BoundaryError when a required field is missing', async () => {
    const row = rowWire();
    delete row['add'];
    invokeMock.mockResolvedValueOnce(listedWire({ rows: [row] }));

    await expect(invokeListMcpRegistrations(7)).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when a field has the wrong type', async () => {
    invokeMock.mockResolvedValueOnce(
      listedWire({ rows: [rowWire({ add: commandWire({ program_at: '0' }) })] })
    );

    await expect(invokeListMcpRegistrations(7)).rejects.toThrow(BoundaryError);
  });
});

describe('invokeRunMcpRegistration', () => {
  it('calls run_mcp_registration with the profile, the action and the request id', async () => {
    invokeMock.mockResolvedValueOnce({ request_id: 8, profile: 'alpha-cli', action: 'update' });

    await invokeRunMcpRegistration('alpha-cli', 'update', 8);

    expect(invokeMock).toHaveBeenCalledWith('run_mcp_registration', {
      profile: 'alpha-cli',
      action: 'update',
      request_id: 8,
    });
  });

  it('narrows the acknowledgement into camelCase', async () => {
    invokeMock.mockResolvedValueOnce({ request_id: 8, profile: 'alpha-cli', action: 'remove' });

    const requested: McpRegistrationRequested = await invokeRunMcpRegistration(
      'alpha-cli',
      'remove',
      8
    );

    expect(requested).toEqual({ requestId: 8, profile: 'alpha-cli', action: 'remove' });
  });

  it('rejects with BoundaryError when the reply names an action it does not know', async () => {
    invokeMock.mockResolvedValueOnce({ request_id: 8, profile: 'alpha-cli', action: 'reinstall' });

    await expect(invokeRunMcpRegistration('alpha-cli', 'register', 8)).rejects.toThrow(
      BoundaryError
    );
  });
});

describe('listenMcpRegistrationStatus', () => {
  it('subscribes to session://mcp-registration-status', async () => {
    await listenMcpRegistrationStatus(vi.fn(), vi.fn());

    expect(listenMock).toHaveBeenCalledWith(STATUS_EVENT, expect.any(Function));
  });

  it('delivers a narrowed payload to the handler', async () => {
    const onStatus = vi.fn<(payload: McpRegistrationStatusPayload) => void>();
    await listenMcpRegistrationStatus(onStatus, vi.fn());
    const callback = listenMock.mock.calls[0]?.[1] as ListenCallback;

    callback({ payload: statusPayloadWire() });

    const expected: McpRegistrationStatusPayload = {
      requestId: 7,
      status: {
        profile: 'alpha-cli',
        state: 'differs',
        differsIn: ['command'],
        reason: null,
        add: {
          argv: ['alpha', 'mcp', 'add', 'yagura', '--', 'C:\\bridge\\yagura-mcp.exe'],
          programAt: 0,
        },
        remove: null,
      },
    };
    expect(onStatus).toHaveBeenCalledWith(expected);
  });

  it('reads a state this build does not know as unknown', async () => {
    const onStatus = vi.fn<(payload: McpRegistrationStatusPayload) => void>();
    await listenMcpRegistrationStatus(onStatus, vi.fn());
    const callback = listenMock.mock.calls[0]?.[1] as ListenCallback;

    callback({
      payload: statusPayloadWire({
        status: statusInfoWire({ state: 'half_registered', reason: 'unreadable output' }),
      }),
    });

    const payload = onStatus.mock.calls[0]?.[0];
    expect(payload?.status.state).toBe('unknown');
    expect(payload?.status.reason).toBe('unreadable output');
  });

  it('reports a payload missing a required field to onBoundaryError instead of the handler', async () => {
    const onStatus = vi.fn<(payload: McpRegistrationStatusPayload) => void>();
    const onBoundaryError = vi.fn<(error: unknown) => void>();
    await listenMcpRegistrationStatus(onStatus, onBoundaryError);
    const callback = listenMock.mock.calls[0]?.[1] as ListenCallback;
    const status = statusInfoWire();
    delete status['differs_in'];

    callback({ payload: statusPayloadWire({ status }) });

    expect(onStatus).not.toHaveBeenCalled();
    expect(onBoundaryError).toHaveBeenCalledWith(expect.any(BoundaryError));
  });
});

describe('listenMcpRegistrationResult', () => {
  it('subscribes to session://mcp-registration-result', async () => {
    await listenMcpRegistrationResult(vi.fn(), vi.fn());

    expect(listenMock).toHaveBeenCalledWith(RESULT_EVENT, expect.any(Function));
  });

  it('delivers a narrowed payload to the handler', async () => {
    const onResult = vi.fn<(payload: McpRegistrationResultPayload) => void>();
    await listenMcpRegistrationResult(onResult, vi.fn());
    const callback = listenMock.mock.calls[0]?.[1] as ListenCallback;

    callback({ payload: resultPayloadWire({ error: 'the add step failed' }) });

    const expected: McpRegistrationResultPayload = {
      requestId: 8,
      profile: 'alpha-cli',
      action: 'register',
      steps: [
        {
          argv: ['alpha', 'mcp', 'add', 'yagura', '--', 'C:\\bridge\\yagura-mcp.exe'],
          programAt: 0,
          exitCode: 0,
          output: 'Added.',
        },
        { argv: [], programAt: 0, exitCode: null, output: 'set' },
      ],
      error: 'the add step failed',
      status: {
        profile: 'alpha-cli',
        state: 'registered',
        differsIn: [],
        reason: null,
        add: {
          argv: ['alpha', 'mcp', 'add', 'yagura', '--', 'C:\\bridge\\yagura-mcp.exe'],
          programAt: 0,
        },
        remove: null,
      },
    };
    expect(onResult).toHaveBeenCalledWith(expected);
  });

  it('reports an action it does not know to onBoundaryError', async () => {
    const onResult = vi.fn<(payload: McpRegistrationResultPayload) => void>();
    const onBoundaryError = vi.fn<(error: unknown) => void>();
    await listenMcpRegistrationResult(onResult, onBoundaryError);
    const callback = listenMock.mock.calls[0]?.[1] as ListenCallback;

    callback({ payload: resultPayloadWire({ action: 'reinstall' }) });

    expect(onResult).not.toHaveBeenCalled();
    expect(onBoundaryError).toHaveBeenCalledWith(expect.any(BoundaryError));
  });

  it('reports a step field of the wrong type to onBoundaryError', async () => {
    const onResult = vi.fn<(payload: McpRegistrationResultPayload) => void>();
    const onBoundaryError = vi.fn<(error: unknown) => void>();
    await listenMcpRegistrationResult(onResult, onBoundaryError);
    const callback = listenMock.mock.calls[0]?.[1] as ListenCallback;

    callback({ payload: resultPayloadWire({ steps: [stepWire({ exit_code: '0' })] }) });

    expect(onResult).not.toHaveBeenCalled();
    expect(onBoundaryError).toHaveBeenCalledWith(expect.any(BoundaryError));
  });
});
