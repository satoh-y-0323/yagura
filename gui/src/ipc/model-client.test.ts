/**
 * TS-boundary contract for the model-alias bridge: the two Tauri commands
 * the WebView calls (`list_model_choices`, `save_model_aliases`) and the
 * subscription
 * to `session://model-choices` (`SESSION_EVENT.modelChoices`).
 *
 * `@tauri-apps/api/core` and `@tauri-apps/api/event` are mocked, so every
 * assertion is about the calls this module makes and the shape it produces,
 * not about a real Tauri runtime. Lives next to `model-boundary.test.ts`, the
 * way `plan-client.test.ts` sits next to `plan-boundary.test.ts`.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const { invokeMock, listenMock, unlistenMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listenMock: vi.fn(),
  unlistenMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: listenMock,
}));

import { BoundaryError, SESSION_EVENT } from './session-boundary';
import { invokeListModelChoices, invokeSaveModelAliases, listenModelChoices } from './model-client';

/** A Tauri event callback, as registered through the mocked `listen`. */
type ListenCallback = (event: { payload: unknown }) => void;

/** A well-formed `LaunchOptionAlias` wire row. */
function launchOptionAliasWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    alias: 'fast',
    value: 'haiku',
    ...overrides,
  };
}

/** A well-formed `ModelChoices` push wire value. */
function modelChoicesPayloadWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    request_id: 7,
    profile: 'claude-code',
    source: { kind: 'values' },
    choices: [{ id: 'haiku', label: 'Haiku' }],
    error: null,
    ...overrides,
  };
}

beforeEach(() => {
  invokeMock.mockReset();
  listenMock.mockReset();
  unlistenMock.mockReset();
  listenMock.mockResolvedValue(unlistenMock);
});

describe('SESSION_EVENT.modelChoices', () => {
  it('names the model-choices push', () => {
    expect(SESSION_EVENT.modelChoices).toBe('session://model-choices');
  });
});

describe('invokeListModelChoices', () => {
  it('calls list_model_choices with the profile and the caller-chosen request id', async () => {
    invokeMock.mockResolvedValueOnce({ request_id: 7, profile: 'claude-code' });

    await invokeListModelChoices('claude-code', 7);

    expect(invokeMock).toHaveBeenCalledWith('list_model_choices', {
      profile: 'claude-code',
      request_id: 7,
    });
  });

  it('narrows the immediate acknowledgement', async () => {
    invokeMock.mockResolvedValueOnce({ request_id: 7, profile: 'claude-code' });

    const result = await invokeListModelChoices('claude-code', 7);

    expect(result).toEqual({ request_id: 7, profile: 'claude-code' });
  });

  it('rejects with BoundaryError when the reply fails validation', async () => {
    invokeMock.mockResolvedValueOnce({ request_id: 'seven', profile: 'claude-code' });

    await expect(invokeListModelChoices('claude-code', 7)).rejects.toThrow(BoundaryError);
  });
});

describe('invokeSaveModelAliases', () => {
  it('calls save_model_aliases with the profile and the aliases to store', async () => {
    invokeMock.mockResolvedValueOnce({
      profile: 'claude-code',
      aliases: [launchOptionAliasWire()],
    });

    await invokeSaveModelAliases('claude-code', [{ alias: 'fast', value: 'haiku' }]);

    expect(invokeMock).toHaveBeenCalledWith('save_model_aliases', {
      profile: 'claude-code',
      aliases: [{ alias: 'fast', value: 'haiku' }],
    });
  });

  it('narrows the reply into the stored aliases', async () => {
    invokeMock.mockResolvedValueOnce({
      profile: 'claude-code',
      aliases: [launchOptionAliasWire({ value: 'sonnet' })],
    });

    const result = await invokeSaveModelAliases('claude-code', [
      { alias: 'fast', value: 'sonnet' },
    ]);

    expect(result.aliases).toEqual([{ alias: 'fast', value: 'sonnet' }]);
  });

  it('rejects with BoundaryError when the reply fails validation', async () => {
    invokeMock.mockResolvedValueOnce({ profile: 'claude-code', aliases: [{ alias: 'fast' }] });

    await expect(
      invokeSaveModelAliases('claude-code', [{ alias: 'fast', value: 'sonnet' }])
    ).rejects.toThrow(BoundaryError);
  });
});

describe('listenModelChoices', () => {
  it('subscribes to session://model-choices', async () => {
    await listenModelChoices(vi.fn(), vi.fn());

    expect(listenMock).toHaveBeenCalledWith(SESSION_EVENT.modelChoices, expect.any(Function));
  });

  it('delivers a narrowed payload to the handler', async () => {
    const onModelChoices = vi.fn();
    await listenModelChoices(onModelChoices, vi.fn());
    const callback = listenMock.mock.calls[0]?.[1] as ListenCallback;

    callback({ payload: modelChoicesPayloadWire() });

    expect(onModelChoices).toHaveBeenCalledWith(
      expect.objectContaining({ request_id: 7, profile: 'claude-code' })
    );
  });

  it('reports a malformed payload to onBoundaryError instead of the handler', async () => {
    const onModelChoices = vi.fn();
    const onBoundaryError = vi.fn();
    await listenModelChoices(onModelChoices, onBoundaryError);
    const callback = listenMock.mock.calls[0]?.[1] as ListenCallback;

    callback({ payload: { request_id: 'seven', profile: 'claude-code' } });

    expect(onModelChoices).not.toHaveBeenCalled();
    expect(onBoundaryError).toHaveBeenCalledWith(expect.any(BoundaryError));
  });
});
