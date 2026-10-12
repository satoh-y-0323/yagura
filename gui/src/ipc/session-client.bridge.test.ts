/**
 * The session client's thin commands and its event subscription: which Tauri
 * command each call invokes with which arguments, how a reply is read, and
 * how each pushed event reaches its handler (or the boundary-error sink).
 *
 * `@tauri-apps/api/core` and `@tauri-apps/api/event` are mocked, so every
 * assertion is about the calls this module makes and what it hands back.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const { invokeMock, listenMock, unlistenMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listenMock: vi.fn(),
  unlistenMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: invokeMock }));
vi.mock('@tauri-apps/api/event', () => ({ listen: listenMock }));

import { BoundaryError, SESSION_EVENT, type Frame } from './session-boundary';
import {
  invokeClearWindowSelection,
  invokeCloseWindow,
  invokeCopyWindowSelection,
  invokeDecideApproval,
  invokeGetApprovalSettings,
  invokeGetRecoveryStatus,
  invokeKeymapFilePath,
  invokeListDetections,
  invokeListProfiles,
  invokeListRestoreCandidates,
  invokeLoadKeymap,
  invokeLoadThemes,
  invokeNextFrames,
  invokeOpenWindow,
  invokeReconnectDaemon,
  invokeRenameWindow,
  invokeSaveKeymap,
  invokeSetProfileEnabled,
  invokeSetWindowPalette,
  invokeSetWindowVisible,
  subscribeSession,
  subscribeWindowList,
  type AppEventHandlers,
  type WindowEventHandlers,
  type WirePalette,
} from './session-client';

type ListenCallback = (event: { payload: unknown }) => void;
type Wire = Record<string, unknown>;

const handlersByEvent = new Map<string, ListenCallback>();

beforeEach(() => {
  invokeMock.mockReset();
  listenMock.mockReset();
  unlistenMock.mockReset();
  handlersByEvent.clear();
  listenMock.mockImplementation((name: string, callback: ListenCallback) => {
    handlersByEvent.set(name, callback);
    return Promise.resolve(unlistenMock);
  });
});

async function messageOf(promise: Promise<unknown>): Promise<string> {
  try {
    await promise;
  } catch (error) {
    expect(error).toBeInstanceOf(BoundaryError);
    return (error as Error).message;
  }
  throw new Error('the reply was accepted');
}

function frameWire(seq: number): Wire {
  return {
    seq,
    cols: 2,
    rows: 1,
    reset: false,
    last: true,
    attrs: [],
    lines: [],
    cursor: { row: 0, col: 0, visible: true, shape: 'block' },
    view: { display_offset: 0, history_rows: 0, mouse_reporting: false, alt_screen: false },
  };
}

describe('the thin window commands', () => {
  const palette: WirePalette = {
    foreground: [1, 2, 3],
    background: [4, 5, 6],
    cursor: [7, 8, 9],
    ansi: [],
  };

  it('sends the palette and the visibility of a window by name', async () => {
    invokeMock.mockResolvedValue(undefined);
    await invokeSetWindowPalette('w', palette);
    await invokeSetWindowVisible('w', false);
    expect(invokeMock).toHaveBeenNthCalledWith(1, 'set_window_palette', {
      window_id: 'w',
      palette,
    });
    expect(invokeMock).toHaveBeenNthCalledWith(2, 'set_window_visible', {
      window_id: 'w',
      visible: false,
    });
  });

  it('reads the selection of a window as text or null, and clears it by window', async () => {
    invokeMock.mockResolvedValueOnce('picked');
    await expect(invokeCopyWindowSelection('w')).resolves.toBe('picked');
    expect(invokeMock).toHaveBeenLastCalledWith('copy_window_selection', { window_id: 'w' });
    invokeMock.mockResolvedValueOnce(null);
    await expect(invokeCopyWindowSelection('w')).resolves.toBeNull();
    invokeMock.mockResolvedValueOnce(5);
    await expect(invokeCopyWindowSelection('w')).rejects.toBeInstanceOf(BoundaryError);
    invokeMock.mockResolvedValueOnce(undefined);
    await invokeClearWindowSelection('w');
    expect(invokeMock).toHaveBeenLastCalledWith('clear_window_selection', { window_id: 'w' });
  });

  it('answers an approval with the request id and the verdict', async () => {
    invokeMock.mockResolvedValue('decided');
    await expect(invokeDecideApproval('r1', true)).resolves.toBe('decided');
    expect(invokeMock).toHaveBeenCalledWith('decide_approval', {
      request_id: 'r1',
      approved: true,
    });
  });

  it('asks for the recovery status and for a reconnect by command name', async () => {
    invokeMock.mockResolvedValueOnce({ status: 'retrying', rev: 3 });
    await expect(invokeGetRecoveryStatus()).resolves.toMatchObject({ status: 'retrying', rev: 3 });
    expect(invokeMock).toHaveBeenLastCalledWith('get_recovery_status');
    invokeMock.mockResolvedValueOnce(undefined);
    await invokeReconnectDaemon();
    expect(invokeMock).toHaveBeenLastCalledWith('reconnect_daemon');
  });

  it('renames a window and returns the stored name', async () => {
    invokeMock.mockResolvedValueOnce('stored');
    await expect(invokeRenameWindow('w', 'asked')).resolves.toBe('stored');
    expect(invokeMock).toHaveBeenLastCalledWith('rename_window', { window_id: 'w', name: 'asked' });
    invokeMock.mockResolvedValueOnce(4);
    expect(await messageOf(invokeRenameWindow('w', 'x'))).toBe('rename_window: expected a string');
  });

  it('closes a window with the force flag it was given', async () => {
    invokeMock.mockResolvedValue('closed');
    await invokeCloseWindow('w', true);
    expect(invokeMock).toHaveBeenCalledWith('close_window', { window_id: 'w', force: true });
  });
});

describe('invokeNextFrames', () => {
  it('sends the acknowledgements and pairs each frame with its window', async () => {
    invokeMock.mockResolvedValue([
      ['a', frameWire(1)],
      ['b', frameWire(2)],
    ]);
    const onInvalid = vi.fn();
    const frames = await invokeNextFrames([{ window_id: 'a', seq: 0 }], onInvalid);
    expect(invokeMock).toHaveBeenCalledWith('next_frames', { acks: [{ window_id: 'a', seq: 0 }] });
    expect(frames.map(([id, frame]: [string, Frame]) => [id, frame.seq])).toEqual([
      ['a', 1],
      ['b', 2],
    ]);
    expect(onInvalid).not.toHaveBeenCalled();
  });

  it('reports a part that fails validation and returns the rest', async () => {
    invokeMock.mockResolvedValue([
      ['a', frameWire(1)],
      ['b', { seq: 'x' }],
      [7, frameWire(2)],
      'not-a-pair',
      ['c', frameWire(3)],
    ]);
    const errors: unknown[] = [];
    const frames = await invokeNextFrames([], (error) => errors.push(error));
    expect(frames.map(([id]) => id)).toEqual(['a', 'c']);
    expect(errors).toHaveLength(3);
    expect(errors.every((error) => error instanceof BoundaryError)).toBe(true);
    expect((errors[0] as Error).message).toMatch(/^next_frames\.frame\.seq/);
    expect((errors[1] as Error).message).toBe('next_frames.entry: expected a window id');
    expect((errors[2] as Error).message).toMatch(/^next_frames\.entry: expected an array/);
  });

  it('refuses a reply that is not a list', async () => {
    invokeMock.mockResolvedValue({});
    expect(await messageOf(invokeNextFrames([], vi.fn()))).toBe('next_frames: expected an array');
  });
});

describe('the restore candidates command', () => {
  it('keeps the candidates and when they were recorded', async () => {
    invokeMock.mockResolvedValue({
      windows: [{ position: 0, command: ['a'], cwd: '/', name: 'n' }],
      taken_at_epoch_s: 99,
    });
    const result = await invokeListRestoreCandidates();
    expect(invokeMock).toHaveBeenCalledWith('list_restore_candidates');
    expect(result.candidates).toHaveLength(1);
    expect(result.takenAtEpochS).toBe(99);
  });

  it('names the command when the reply is wrong', async () => {
    invokeMock.mockResolvedValue(5);
    expect(await messageOf(invokeListRestoreCandidates())).toMatch(/^list_restore_candidates/);
    invokeMock.mockResolvedValue({ windows: [], taken_at_epoch_s: 'x' });
    expect(await messageOf(invokeListRestoreCandidates())).toMatch(
      /^list_restore_candidates\.taken_at_epoch_s: expected an integer/
    );
  });
});

describe('the approval settings reply', () => {
  it('names the part that is wrong', async () => {
    invokeMock.mockResolvedValue(5);
    expect(await messageOf(invokeGetApprovalSettings())).toMatch(/^approval_settings/);
    const settings = (overrides: Wire): Wire => ({
      playbook: 'loaded',
      tools: [],
      plans_require_approval: false,
      ...overrides,
    });
    invokeMock.mockResolvedValue(settings({ tools: 5 }));
    expect(await messageOf(invokeGetApprovalSettings())).toBe(
      'approval_settings.tools: expected an array'
    );
    invokeMock.mockResolvedValue(settings({ tools: [5] }));
    expect(await messageOf(invokeGetApprovalSettings())).toBe(
      'approval_settings.tools: expected an object'
    );
  });
});

function profileWire(overrides: Wire = {}): Wire {
  return {
    id: 'p',
    display_name: 'P',
    detection: 'det',
    detection_warning: 'warn',
    cwd: '/work',
    command: ['p'],
    available: true,
    unavailable_reason: 'why',
    model: { aliases: [{ alias: 'fast', value: 'm' }] },
    effort: null,
    enabled: false,
    ...overrides,
  };
}

describe('invokeListProfiles', () => {
  it('keeps every field of a profile', async () => {
    invokeMock.mockResolvedValue({ profiles: [profileWire()], warnings: ['w'] });
    const result = await invokeListProfiles();
    expect(invokeMock).toHaveBeenCalledWith('list_profiles');
    expect(result.profiles[0]).toEqual({
      id: 'p',
      displayName: 'P',
      detection: 'det',
      detectionWarning: 'warn',
      cwd: '/work',
      command: ['p'],
      available: true,
      reason: 'why',
      model: { aliases: [{ alias: 'fast', value: 'm' }] },
      effort: null,
      enabled: false,
    });
    expect(result.warnings).toEqual(['w']);
  });

  it('reads a missing or null warnings list as empty', async () => {
    invokeMock.mockResolvedValueOnce({ profiles: [] });
    await expect(invokeListProfiles()).resolves.toEqual({ profiles: [], warnings: [] });
    invokeMock.mockResolvedValueOnce({ profiles: [], warnings: null });
    await expect(invokeListProfiles()).resolves.toEqual({ profiles: [], warnings: [] });
  });

  it.each<[string, Wire, RegExp]>([
    ['profiles not an array', { profiles: 5 }, /^list_profiles\.profiles: expected an array/],
    ['a profile not an object', { profiles: [5] }, /^list_profiles\.profiles: expected an object/],
    [
      'available not a boolean',
      { profiles: [profileWire({ available: 'yes' })] },
      /^list_profiles\.profiles\.available: expected a boolean/,
    ],
    [
      'command not an array',
      { profiles: [profileWire({ command: 'p' })] },
      /^list_profiles\.profiles\.command: expected an array/,
    ],
    [
      'command holding a number',
      { profiles: [profileWire({ command: ['p', 1] })] },
      /^list_profiles\.profiles\.command: expected a string entry/,
    ],
    [
      'model not an object',
      { profiles: [profileWire({ model: 5 })] },
      /^list_profiles\.profiles\.model: expected an object or null/,
    ],
    [
      'effort aliases not an array',
      { profiles: [profileWire({ effort: { aliases: 5 } })] },
      /^list_profiles\.profiles\.effort\.aliases: expected an array/,
    ],
    [
      'an alias entry not an object',
      { profiles: [profileWire({ effort: { aliases: [5] } })] },
      /effort\.aliases: expected an object entry/,
    ],
    [
      'an alias without a value',
      { profiles: [profileWire({ effort: { aliases: [{ alias: 'a' }] } })] },
      /effort\.aliases: expected string alias\/value fields/,
    ],
    [
      'an alias without a name',
      { profiles: [profileWire({ effort: { aliases: [{ value: 'a' }] } })] },
      /effort\.aliases: expected string alias\/value fields/,
    ],
    [
      'enabled not a boolean',
      { profiles: [profileWire({ enabled: 'no' })] },
      /^list_profiles\.profiles\.enabled: expected a boolean/,
    ],
    [
      'warnings holding a number',
      { profiles: [], warnings: [1] },
      /^list_profiles\.warnings: expected a string entry/,
    ],
    [
      'warnings not an array',
      { profiles: [], warnings: 'w' },
      /^list_profiles\.warnings: expected an array/,
    ],
    [
      'detection_warning not text',
      { profiles: [profileWire({ detection_warning: 5 })] },
      /^list_profiles\.detection_warning: expected a string/,
    ],
    [
      'cwd not text',
      { profiles: [profileWire({ cwd: 5 })] },
      /^list_profiles\.cwd: expected a string/,
    ],
  ])('names the part when %s', async (_name, reply, pattern) => {
    invokeMock.mockResolvedValue(reply);
    expect(await messageOf(invokeListProfiles())).toMatch(pattern);
  });

  it('reads a profile with no enabled key as enabled', async () => {
    const wire = profileWire();
    delete wire['enabled'];
    invokeMock.mockResolvedValue({ profiles: [wire] });
    expect((await invokeListProfiles()).profiles[0]?.enabled).toBe(true);
  });

  it('names the command when the reply is not an object', async () => {
    invokeMock.mockResolvedValue(5);
    expect(await messageOf(invokeListProfiles())).toMatch(/^list_profiles: expected an object/);
  });
});

describe('the profile and detection commands', () => {
  it('names set_profile_enabled and its enabled flag', async () => {
    invokeMock.mockResolvedValue({ profile: 'p', enabled: 'x' });
    expect(await messageOf(invokeSetProfileEnabled('p', true))).toBe(
      'set_profile_enabled.enabled: expected a boolean'
    );
  });

  it('names list_detections and its names', async () => {
    invokeMock.mockResolvedValueOnce({ names: ['a', 'b'], warning: 'w' });
    await expect(invokeListDetections()).resolves.toEqual({ names: ['a', 'b'], warning: 'w' });
    expect(invokeMock).toHaveBeenLastCalledWith('list_detections');
    invokeMock.mockResolvedValueOnce({ names: 5 });
    expect(await messageOf(invokeListDetections())).toBe(
      'list_detections.names: expected an array'
    );
    invokeMock.mockResolvedValueOnce(5);
    expect(await messageOf(invokeListDetections())).toMatch(/^list_detections/);
  });

  it('names open_window when its reply is wrong', async () => {
    invokeMock.mockResolvedValue({ window_id: 5 });
    expect(await messageOf(invokeOpenWindow({ command: ['x'], cwd: '/' }))).toBe(
      'open_window.window_id: expected a string'
    );
  });
});

describe('the keymap commands', () => {
  it('reads the keymap file path as text or null', async () => {
    invokeMock.mockResolvedValueOnce('/cfg/keymap.toml');
    await expect(invokeKeymapFilePath()).resolves.toBe('/cfg/keymap.toml');
    expect(invokeMock).toHaveBeenLastCalledWith('keymap_file_path');
    invokeMock.mockResolvedValueOnce(null);
    await expect(invokeKeymapFilePath()).resolves.toBeNull();
    invokeMock.mockResolvedValueOnce(5);
    expect(await messageOf(invokeKeymapFilePath())).toBe(
      'keymap_file_path: expected a string or null, got number'
    );
  });

  it('turns a named refusal into a value and anything else into a BoundaryError', async () => {
    invokeMock.mockRejectedValueOnce('too_large');
    await expect(invokeSaveKeymap({})).resolves.toEqual({ ok: false, reason: 'too_large' });
    invokeMock.mockRejectedValueOnce('no_config_root');
    await expect(invokeSaveKeymap({})).resolves.toEqual({ ok: false, reason: 'no_config_root' });
    invokeMock.mockRejectedValueOnce('write_failed');
    await expect(invokeSaveKeymap({})).resolves.toEqual({ ok: false, reason: 'write_failed' });
    invokeMock.mockRejectedValueOnce('exploded');
    expect(await messageOf(invokeSaveKeymap({}))).toBe(
      'save_keymap: unrecognized rejection "exploded"'
    );
    invokeMock.mockRejectedValueOnce(new Error('x'));
    expect(await messageOf(invokeSaveKeymap({}))).toMatch(/^save_keymap: unrecognized rejection/);
    invokeMock.mockResolvedValueOnce(undefined);
    await expect(invokeSaveKeymap({ a: 'b' })).resolves.toEqual({ ok: true });
    expect(invokeMock).toHaveBeenLastCalledWith('save_keymap', { keys: { a: 'b' } });
  });

  it('names load_keymap and the part of its reply that is wrong', async () => {
    invokeMock.mockResolvedValueOnce({ keys: { a: 'Ctrl+A' }, legacy_entries: 2 });
    await expect(invokeLoadKeymap()).resolves.toEqual({
      keys: { a: 'Ctrl+A' },
      legacy_entries: 2,
    });
    expect(invokeMock).toHaveBeenLastCalledWith('load_keymap');
    invokeMock.mockResolvedValueOnce(5);
    expect(await messageOf(invokeLoadKeymap())).toMatch(/^load_keymap: expected an object/);
    invokeMock.mockResolvedValueOnce({ keys: 5, legacy_entries: 0 });
    expect(await messageOf(invokeLoadKeymap())).toMatch(/^load_keymap\.keys: expected an object/);
    invokeMock.mockResolvedValueOnce({ keys: { act: 1 }, legacy_entries: 0 });
    expect(await messageOf(invokeLoadKeymap())).toBe('load_keymap.keys.act: expected a string');
    invokeMock.mockResolvedValueOnce({ keys: {}, legacy_entries: -1 });
    expect(await messageOf(invokeLoadKeymap())).toBe(
      'load_keymap.legacy_entries: expected a non-negative integer'
    );
    invokeMock.mockResolvedValueOnce({ keys: {}, legacy_entries: 0, failure: 'x' });
    expect(await messageOf(invokeLoadKeymap())).toBe(
      'load_keymap.failure: unrecognized failure "x"'
    );
    invokeMock.mockResolvedValueOnce({ keys: {}, legacy_entries: 0, failure: 4 });
    expect(await messageOf(invokeLoadKeymap())).toBe('load_keymap.failure: unrecognized failure 4');
  });
});

describe('invokeLoadThemes refusals', () => {
  function themesWire(overrides: Wire = {}): Wire {
    return { themes: { dark: { bg: '#000' } }, vendor_assignments: { v: 'dark' }, ...overrides };
  }

  it('keeps themes, assignments and the default', async () => {
    invokeMock.mockResolvedValue(themesWire({ default_theme: 'dark' }));
    const result = await invokeLoadThemes();
    expect(invokeMock).toHaveBeenCalledWith('load_themes');
    expect(result.themes['dark']?.['bg']).toBe('#000');
    expect(result.vendorAssignments).toEqual({ v: 'dark' });
    expect(result.defaultTheme).toBe('dark');
  });

  it.each<[string, unknown, string]>([
    ['a reply that is not an object', 5, 'load_themes: expected an object'],
    [
      'themes that are not an object',
      themesWire({ themes: 5 }),
      'load_themes.themes: expected an object',
    ],
    [
      'a theme that is not an object',
      themesWire({ themes: { dark: 5 } }),
      'load_themes.themes.dark: expected an object',
    ],
    [
      'a token that is not text',
      themesWire({ themes: { dark: { bg: 5 } } }),
      'load_themes.themes.dark.bg: expected a string',
    ],
    [
      'assignments that are not an object',
      themesWire({ vendor_assignments: 5 }),
      'load_themes.vendor_assignments: expected an object',
    ],
    [
      'an assignment that is not text',
      themesWire({ vendor_assignments: { v: 5 } }),
      'load_themes.vendor_assignments.v: expected a string',
    ],
    [
      'a default theme that is not text',
      themesWire({ default_theme: 5 }),
      'load_themes.default_theme: expected a string',
    ],
    [
      'an unknown failure',
      themesWire({ failure: 'x' }),
      'load_themes.failure: unrecognized failure "x"',
    ],
    [
      'a failure that is not text',
      themesWire({ failure: 5 }),
      'load_themes.failure: unrecognized failure 5',
    ],
  ])('names the part for %s', async (_name, reply, message) => {
    invokeMock.mockResolvedValue(reply);
    expect(await messageOf(invokeLoadThemes())).toBe(message);
  });
});

describe('subscribeSession routing', () => {
  function appHandlers(overrides: Partial<AppEventHandlers> = {}): AppEventHandlers {
    return {
      onDaemonSpawnWarning: vi.fn(),
      onBoundaryError: vi.fn(),
      ...overrides,
    };
  }

  function windowHandlers(): WindowEventHandlers {
    return {
      onFrame: vi.fn(() => null),
      onExit: vi.fn(),
      onDisconnected: vi.fn(),
      onSuperseded: vi.fn(),
    };
  }

  function push(name: string, payload: unknown): void {
    const handler = handlersByEvent.get(name);
    if (handler === undefined) {
      throw new Error(`no listener for ${name}`);
    }
    handler({ payload });
  }

  it('routes each application event to its handler with the narrowed payload', async () => {
    const app = appHandlers({
      onStatus: vi.fn(),
      onApprovalRequested: vi.fn(),
      onApprovalWithdrawn: vi.fn(),
      onApprovalReset: vi.fn(),
      onApprovalRegistrationRefused: vi.fn(),
      onDaemonRecovery: vi.fn(),
    });
    await subscribeSession(() => undefined, app);

    push(SESSION_EVENT.daemonSpawnWarning, { message: 'm' });
    expect(app.onDaemonSpawnWarning).toHaveBeenCalledWith({ message: 'm' });
    push(SESSION_EVENT.status, { window_id: 'w', status: 'idle' });
    expect(app.onStatus).toHaveBeenCalledWith({ window_id: 'w', detected: { status: 'idle' } });
    push(SESSION_EVENT.approvalRequested, {
      request: {
        request_id: 'r',
        tool: 't',
        requester: { kind: 'external' },
      },
    });
    expect(app.onApprovalRequested).toHaveBeenCalledTimes(1);
    push(SESSION_EVENT.approvalWithdrawn, { request_id: 'r', reason: 'timeout' });
    expect(app.onApprovalWithdrawn).toHaveBeenCalledWith({ request_id: 'r', reason: 'timeout' });
    push(SESSION_EVENT.approvalReset, { requests: [] });
    expect(app.onApprovalReset).toHaveBeenCalledWith({ requests: [] });
    push(SESSION_EVENT.approvalRegistrationRefused, {});
    expect(app.onApprovalRegistrationRefused).toHaveBeenCalledWith({});
    push(SESSION_EVENT.daemonRecovery, { status: 'retrying', rev: 1 });
    expect(app.onDaemonRecovery).toHaveBeenCalledTimes(1);
    expect(app.onBoundaryError).not.toHaveBeenCalled();
  });

  it('accepts every application event when the optional handlers are absent', async () => {
    const app = appHandlers();
    await subscribeSession(() => undefined, app);
    push(SESSION_EVENT.status, { window_id: 'w', status: 'idle' });
    push(SESSION_EVENT.approvalRequested, {
      request: { request_id: 'r', tool: 't', requester: { kind: 'external' } },
    });
    push(SESSION_EVENT.approvalWithdrawn, { request_id: 'r', reason: 'timeout' });
    push(SESSION_EVENT.approvalReset, { requests: [] });
    push(SESSION_EVENT.approvalRegistrationRefused, {});
    push(SESSION_EVENT.daemonRecovery, { status: 'idle', rev: 0 });
    expect(app.onBoundaryError).not.toHaveBeenCalled();
  });

  it('reports a bad application payload to the boundary-error sink and not to its handler', async () => {
    const app = appHandlers({ onStatus: vi.fn(), onDaemonRecovery: vi.fn() });
    await subscribeSession(() => undefined, app);
    push(SESSION_EVENT.status, { status: 'idle' });
    push(SESSION_EVENT.daemonRecovery, 5);
    expect(app.onStatus).not.toHaveBeenCalled();
    expect(app.onDaemonRecovery).not.toHaveBeenCalled();
    expect(app.onBoundaryError).toHaveBeenCalledTimes(2);
  });

  it('routes a window event to the handlers of the window it names', async () => {
    const mine = windowHandlers();
    const app = appHandlers();
    await subscribeSession((id) => (id === 'w' ? mine : undefined), app);

    push(SESSION_EVENT.exit, { window_id: 'w', exit_code: 3 });
    push(SESSION_EVENT.disconnected, { window_id: 'w', reason: 'gone' });
    push(SESSION_EVENT.superseded, { window_id: 'w', reason: 'taken' });
    expect(mine.onExit).toHaveBeenCalledWith({ window_id: 'w', exit_code: 3 });
    expect(mine.onDisconnected).toHaveBeenCalledWith({ window_id: 'w', reason: 'gone' });
    expect(mine.onSuperseded).toHaveBeenCalledWith({ window_id: 'w', reason: 'taken' });
  });

  it('drops a window event for a window nobody holds, and reports a bad one', async () => {
    const app = appHandlers();
    await subscribeSession(() => undefined, app);
    push(SESSION_EVENT.exit, { window_id: 'ghost', exit_code: 0 });
    expect(app.onBoundaryError).not.toHaveBeenCalled();
    push(SESSION_EVENT.exit, { exit_code: 0 });
    push(SESSION_EVENT.disconnected, 5);
    push(SESSION_EVENT.superseded, null);
    expect(app.onBoundaryError).toHaveBeenCalledTimes(3);
  });

  it('cancels every subscription it made', async () => {
    const unlisten = await subscribeSession(() => undefined, appHandlers());
    expect(listenMock).toHaveBeenCalledTimes(handlersByEvent.size);
    expect(unlistenMock).not.toHaveBeenCalled();
    unlisten();
    expect(unlistenMock).toHaveBeenCalledTimes(handlersByEvent.size);
  });
});

describe('subscribeWindowList', () => {
  it('listens to the window list the core polls and reports a bad snapshot', async () => {
    const onList = vi.fn();
    const onBoundaryError = vi.fn();
    await subscribeWindowList(onList, onBoundaryError);
    const handler = handlersByEvent.get('session://window-list');
    expect(handler).toBeDefined();
    handler?.({ payload: { windows: [], protocol_revision: 16 } });
    expect(onList).toHaveBeenCalledWith({ windows: [], protocol_revision: 16 });
    handler?.({ payload: 5 });
    expect(onBoundaryError).toHaveBeenCalledTimes(1);
    expect(onList).toHaveBeenCalledTimes(1);
  });
});
