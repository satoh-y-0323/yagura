/**
 * Window-scoped IPC client contract.
 *
 * `@tauri-apps/api/core` and `@tauri-apps/api/event` are mocked so every
 * assertion here is about the shape of the calls this module makes, not
 * about a real Tauri runtime.
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
import {
  invokeAttachWindow,
  invokeSendWindowInput,
  invokeResize,
  invokeListWindows,
  invokeOpenWindow,
  invokeCloseWindow,
  invokeInterruptWindow,
  invokeRenameWindow,
  invokeListProfiles,
  invokeSetProfileEnabled,
  invokeGetApprovalSettings,
  invokeSetAutoApprove,
  invokeSetPlanApprovalRequired,
  subscribeSession,
  type ApprovalSettingsInfo,
  type WindowEventHandlers,
  type AppEventHandlers,
} from './session-client';

/** A Tauri event callback, as registered through the mocked `listen`. */
type ListenCallback = (event: { payload: unknown }) => void;

const WINDOW_A = 'w1-k3f9xa';
const WINDOW_B = 'w2-k3f9xa';

/** A `session://exit` payload for window A. */
const EXIT_PAYLOAD = { window_id: WINDOW_A, exit_code: 0 };

/** Captures the callback each event name was registered with. */
const registeredHandlers = new Map<string, ListenCallback>();

beforeEach(() => {
  invokeMock.mockReset();
  listenMock.mockReset();
  unlistenMock.mockReset();
  registeredHandlers.clear();
  listenMock.mockImplementation((name: string, callback: ListenCallback) => {
    registeredHandlers.set(name, callback);
    return Promise.resolve(unlistenMock);
  });
});

interface WrapperCase {
  label: string;
  resolvedValue: unknown;
  invoke: () => Promise<unknown>;
  expectedCommand: string;
  expectedArgs: Record<string, unknown>;
  expectedReturn: unknown;
}

const WRAPPER_CASES: WrapperCase[] = [
  {
    label: 'invokeAttachWindow',
    resolvedValue: { child_exited: false, exit_code: null },
    invoke: () => invokeAttachWindow(WINDOW_A, null),
    expectedCommand: 'attach_window',
    expectedArgs: { window_id: WINDOW_A, size: null },
    expectedReturn: { child_exited: false, exit_code: null },
  },
  {
    label: 'invokeSendWindowInput',
    resolvedValue: undefined,
    invoke: () => invokeSendWindowInput(WINDOW_A, { type: 'text', text: 'ls' }),
    expectedCommand: 'send_window_input',
    expectedArgs: { window_id: WINDOW_A, input: { type: 'text', text: 'ls' } },
    expectedReturn: undefined,
  },
  {
    label: 'invokeResize',
    resolvedValue: undefined,
    invoke: () => invokeResize(WINDOW_A, 80, 24),
    expectedCommand: 'resize',
    expectedArgs: { window_id: WINDOW_A, cols: 80, rows: 24 },
    expectedReturn: undefined,
  },
  {
    label: 'invokeCloseWindow',
    resolvedValue: 'closed',
    invoke: () => invokeCloseWindow(WINDOW_A),
    expectedCommand: 'close_window',
    expectedArgs: { window_id: WINDOW_A },
    expectedReturn: 'closed',
  },
  {
    label: 'invokeInterruptWindow',
    resolvedValue: undefined,
    invoke: () => invokeInterruptWindow(WINDOW_A),
    expectedCommand: 'interrupt_window',
    expectedArgs: { window_id: WINDOW_A },
    expectedReturn: undefined,
  },
  {
    label: 'invokeRenameWindow',
    resolvedValue: 'renamed-window',
    invoke: () => invokeRenameWindow(WINDOW_A, 'renamed-window'),
    expectedCommand: 'rename_window',
    expectedArgs: { window_id: WINDOW_A, name: 'renamed-window' },
    expectedReturn: 'renamed-window',
  },
];

describe.each(WRAPPER_CASES)('$label', (testCase) => {
  it('sends window_id as part of the Tauri command call', async () => {
    invokeMock.mockResolvedValueOnce(testCase.resolvedValue);

    const result = await testCase.invoke();

    expect(invokeMock).toHaveBeenCalledWith(
      testCase.expectedCommand,
      expect.objectContaining(testCase.expectedArgs)
    );
    expect(result).toEqual(testCase.expectedReturn);
  });
});

describe('invokeListWindows and invokeOpenWindow', () => {
  it('do not require a window id and narrow each WindowInfo entry', async () => {
    const rawWindow = {
      window_id: WINDOW_A,
      name: 'claude',
      vendor: 'claude-code',
      cwd: 'C:\\Users\\dev\\project',
      status: 'running',
      exit_code: null,
    };
    invokeMock.mockResolvedValueOnce([rawWindow]);

    const windows = await invokeListWindows();

    expect(invokeMock).toHaveBeenCalledWith('list_windows');
    const { status, ...rest } = rawWindow;
    expect(windows).toEqual([{ ...rest, detected: { status } }]);

    // The reply carries detection_warning alongside window_id, so a caller
    // sees the warning without a follow-up round trip.
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_A, detection_warning: null });
    const opened = await invokeOpenWindow({ command: ['claude'], cwd: 'C:\\Users\\dev\\project' });

    expect(invokeMock).toHaveBeenCalledWith('open_window', {
      command: ['claude'],
      cwd: 'C:\\Users\\dev\\project',
    });
    expect(opened).toEqual({ windowId: WINDOW_A, detectionWarning: null });

    // A malformed entry must fail the boundary rather than be handed on.
    invokeMock.mockResolvedValueOnce([{ ...rawWindow, window_id: 1 }]);
    await expect(invokeListWindows()).rejects.toThrow(BoundaryError);
  });
});

/**
 * The optional `profile` and `vendor` fields of `invokeOpenWindow`'s request
 * follow the same "omitted, not null, when absent or empty" rule the `name` /
 * `detection` fields use.
 */
describe('invokeOpenWindow: profile / vendor fields', () => {
  const WINDOW_ID = 'w1-k3f9xa';

  it('sends profile as part of the open_window call when provided, alongside an empty command', async () => {
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID, detection_warning: null });

    await invokeOpenWindow({ command: [], profile: 'codex-cli' });

    expect(invokeMock).toHaveBeenCalledWith('open_window', {
      command: [],
      cwd: undefined,
      profile: 'codex-cli',
    });
  });

  it('sends vendor as part of the open_window call when provided, without disturbing command/cwd/name/detection', async () => {
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID, detection_warning: null });

    await invokeOpenWindow({
      command: ['claude'],
      cwd: 'C:\\work',
      name: 'my-window',
      detection: 'claude-code',
      vendor: 'claude-code',
    });

    expect(invokeMock).toHaveBeenCalledWith('open_window', {
      command: ['claude'],
      cwd: 'C:\\work',
      name: 'my-window',
      detection: 'claude-code',
      vendor: 'claude-code',
    });
  });

  it('sends both profile and vendor together when both are provided', async () => {
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID, detection_warning: null });

    await invokeOpenWindow({
      command: [],
      cwd: 'C:\\work',
      profile: 'codex-cli',
      vendor: 'codex-cli',
    });

    expect(invokeMock).toHaveBeenCalledWith('open_window', {
      command: [],
      cwd: 'C:\\work',
      profile: 'codex-cli',
      vendor: 'codex-cli',
    });
  });

  it.each([
    ['empty profile', '', undefined],
    ['empty vendor', undefined, ''],
  ])(
    'omits the field entirely rather than sending it as an empty string (%s)',
    async (_label, profile, vendor) => {
      invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID, detection_warning: null });

      await invokeOpenWindow({ command: ['claude'], profile, vendor });

      const [, args] = invokeMock.mock.calls[0] as [string, Record<string, unknown>];
      expect(Object.prototype.hasOwnProperty.call(args, 'profile')).toBe(false);
      expect(Object.prototype.hasOwnProperty.call(args, 'vendor')).toBe(false);
    }
  );

  it('omits profile and vendor entirely when the call supplies neither (existing call sites without them are unaffected)', async () => {
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID, detection_warning: null });

    await invokeOpenWindow({ command: ['claude'], cwd: 'C:\\work' });

    expect(invokeMock).toHaveBeenCalledWith('open_window', {
      command: ['claude'],
      cwd: 'C:\\work',
    });
  });
});

/**
 * The optional `model` and `effort` fields of `invokeOpenWindow`'s request
 * follow the same "omitted, not null, when absent or empty" rule.
 */
describe('invokeOpenWindow: model / effort fields', () => {
  const WINDOW_ID = 'w1-k3f9xa';

  it('sends model as part of the open_window call when provided', async () => {
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID, detection_warning: null });

    await invokeOpenWindow({ command: ['claude'], model: 'strong' });

    expect(invokeMock).toHaveBeenCalledWith('open_window', {
      command: ['claude'],
      cwd: undefined,
      model: 'strong',
    });
  });

  it('sends effort as part of the open_window call when provided', async () => {
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID, detection_warning: null });

    await invokeOpenWindow({ command: ['claude'], effort: 'high' });

    expect(invokeMock).toHaveBeenCalledWith('open_window', {
      command: ['claude'],
      cwd: undefined,
      effort: 'high',
    });
  });

  it('sends both model and effort together, alongside profile and vendor', async () => {
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID, detection_warning: null });

    await invokeOpenWindow({
      command: [],
      cwd: 'C:\\work',
      profile: 'codex-cli',
      vendor: 'codex-cli',
      model: 'strong',
      effort: 'high',
    });

    expect(invokeMock).toHaveBeenCalledWith('open_window', {
      command: [],
      cwd: 'C:\\work',
      profile: 'codex-cli',
      vendor: 'codex-cli',
      model: 'strong',
      effort: 'high',
    });
  });

  it.each([
    ['empty model', '', undefined],
    ['empty effort', undefined, ''],
  ])(
    'omits the field entirely rather than sending it as an empty string (%s)',
    async (_label, model, effort) => {
      invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID, detection_warning: null });

      await invokeOpenWindow({ command: ['claude'], model, effort });

      const [, args] = invokeMock.mock.calls[0] as [string, Record<string, unknown>];
      expect(Object.prototype.hasOwnProperty.call(args, 'model')).toBe(false);
      expect(Object.prototype.hasOwnProperty.call(args, 'effort')).toBe(false);
    }
  );

  it('omits model and effort entirely when the call supplies neither (existing call sites without them are unaffected)', async () => {
    invokeMock.mockResolvedValueOnce({ window_id: WINDOW_ID, detection_warning: null });

    await invokeOpenWindow({ command: ['claude'], cwd: 'C:\\work' });

    expect(invokeMock).toHaveBeenCalledWith('open_window', {
      command: ['claude'],
      cwd: 'C:\\work',
    });
  });
});

/**
 * `list_profiles` narrows the daemon's `ProfileInfo` list
 * (`crates/yagura-protocol/src/lib.rs`) into the camelCase shape the dialog
 * consumes. Field names on the wire stay the daemon's own
 * (`display_name`, `detection_warning`, `unavailable_reason`), matching how
 * every other reply in this module is narrowed.
 */
describe('invokeListProfiles', () => {
  const RAW_PROFILE = {
    id: 'codex-cli',
    display_name: 'Codex CLI',
    detection: 'codex-cli',
    detection_warning: null,
    cwd: null,
    available: true,
    unavailable_reason: null,
    command: ['codex'],
  };

  it('invokes list_profiles with no arguments', async () => {
    invokeMock.mockResolvedValueOnce({ profiles: [], warnings: [] });

    await invokeListProfiles();

    expect(invokeMock).toHaveBeenCalledWith('list_profiles');
  });

  it('narrows a profile entry into camelCase field names', async () => {
    invokeMock.mockResolvedValueOnce({ profiles: [RAW_PROFILE], warnings: [] });

    const result = await invokeListProfiles();

    expect(result.profiles).toEqual([
      {
        id: 'codex-cli',
        displayName: 'Codex CLI',
        detection: 'codex-cli',
        detectionWarning: null,
        cwd: null,
        available: true,
        reason: null,
        command: ['codex'],
        model: null,
        effort: null,
        enabled: true,
      },
    ]);
  });

  it('narrows an unavailable profile, carrying its reason through', async () => {
    invokeMock.mockResolvedValueOnce({
      profiles: [
        {
          ...RAW_PROFILE,
          id: 'zz-unavailable',
          available: false,
          unavailable_reason: 'executable not found',
        },
      ],
      warnings: [],
    });

    const result = await invokeListProfiles();

    expect(result.profiles[0]).toMatchObject({ available: false, reason: 'executable not found' });
  });

  it('returns the warnings array unchanged', async () => {
    invokeMock.mockResolvedValueOnce({
      profiles: [],
      warnings: ["excluded 'Bad Id.toml': invalid identifier"],
    });

    const result = await invokeListProfiles();

    expect(result.warnings).toEqual(["excluded 'Bad Id.toml': invalid identifier"]);
  });

  it('defaults warnings to an empty array when the reply omits the key', async () => {
    invokeMock.mockResolvedValueOnce({ profiles: [] });

    const result = await invokeListProfiles();

    expect(result.warnings).toEqual([]);
  });

  it('rejects with BoundaryError when profiles is not an array', async () => {
    invokeMock.mockResolvedValueOnce({ profiles: 'codex-cli', warnings: [] });

    await expect(invokeListProfiles()).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when a profile entry is missing display_name', async () => {
    const withoutDisplayName: Record<string, unknown> = { ...RAW_PROFILE };
    delete withoutDisplayName['display_name'];
    invokeMock.mockResolvedValueOnce({ profiles: [withoutDisplayName], warnings: [] });

    await expect(invokeListProfiles()).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when a profile entry has a non-boolean available field', async () => {
    invokeMock.mockResolvedValueOnce({
      profiles: [{ ...RAW_PROFILE, available: 'yes' }],
      warnings: [],
    });

    await expect(invokeListProfiles()).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when warnings is present but not an array of strings', async () => {
    invokeMock.mockResolvedValueOnce({ profiles: [], warnings: [42] });

    await expect(invokeListProfiles()).rejects.toThrow(BoundaryError);
  });

  /**
   * `enabled` is the user's on/off choice for the profile. A daemon that
   * predates the field has no way to disable a profile, so a missing key
   * reads as enabled; any present non-boolean is a contract violation.
   */
  describe('enabled', () => {
    it.each([true, false])('carries an explicit enabled: %s through', async (enabled) => {
      invokeMock.mockResolvedValueOnce({ profiles: [{ ...RAW_PROFILE, enabled }], warnings: [] });

      const result = await invokeListProfiles();

      expect(result.profiles[0]?.enabled).toBe(enabled);
    });

    it('reads a missing enabled key as enabled', async () => {
      invokeMock.mockResolvedValueOnce({ profiles: [RAW_PROFILE], warnings: [] });

      const result = await invokeListProfiles();

      expect(result.profiles[0]?.enabled).toBe(true);
    });

    it.each([
      ['a string', 'false'],
      ['a number', 0],
    ])('rejects with BoundaryError when enabled is %s', async (_label, enabled) => {
      invokeMock.mockResolvedValueOnce({ profiles: [{ ...RAW_PROFILE, enabled }], warnings: [] });

      await expect(invokeListProfiles()).rejects.toThrow(BoundaryError);
    });
  });

  /**
   * The daemon sends `command` on every entry -- the unresolved argv it
   * would spawn, empty when there is none for this operating system. Unlike
   * `warnings`, the daemon never omits this key (it is always at least an
   * empty array), so narrowing it is a required field, not an optional one.
   */
  describe('command', () => {
    it('narrows the command array onto the profile entry', async () => {
      invokeMock.mockResolvedValueOnce({
        profiles: [{ ...RAW_PROFILE, command: ['codex', '--flag'] }],
        warnings: [],
      });

      const result = await invokeListProfiles();

      expect(result.profiles[0]).toMatchObject({ command: ['codex', '--flag'] });
    });

    it('narrows an empty command array (no argv for this operating system, or no default shell resolved)', async () => {
      invokeMock.mockResolvedValueOnce({
        profiles: [{ ...RAW_PROFILE, command: [] }],
        warnings: [],
      });

      const result = await invokeListProfiles();

      expect(result.profiles[0]).toMatchObject({ command: [] });
    });

    it('rejects with BoundaryError when a profile entry is missing command', async () => {
      const withoutCommand: Record<string, unknown> = { ...RAW_PROFILE };
      delete withoutCommand['command'];
      invokeMock.mockResolvedValueOnce({ profiles: [withoutCommand], warnings: [] });

      await expect(invokeListProfiles()).rejects.toThrow(BoundaryError);
    });

    it('rejects with BoundaryError when command is present but not an array of strings', async () => {
      invokeMock.mockResolvedValueOnce({
        profiles: [{ ...RAW_PROFILE, command: [42] }],
        warnings: [],
      });

      await expect(invokeListProfiles()).rejects.toThrow(BoundaryError);
    });
  });

  /**
   * `model` / `effort` narrow to a `{ aliases: { alias, value }[] }` spec
   * (protocol `LaunchOptionInfo`). A missing key and an
   * explicit `null` both mean "this profile does not support the axis";
   * only a well-formed object is carried through.
   */
  describe('model and effort launch-option specs', () => {
    it('narrows an explicit null model/effort to null', async () => {
      invokeMock.mockResolvedValueOnce({
        profiles: [{ ...RAW_PROFILE, model: null, effort: null }],
        warnings: [],
      });

      const result = await invokeListProfiles();

      expect(result.profiles[0]).toMatchObject({ model: null, effort: null });
    });

    it('narrows a present model/effort spec, keeping alias order as given', async () => {
      const spec = {
        aliases: [
          { alias: 'alpha', value: 'tier-a' },
          { alias: 'beta', value: 'tier-b' },
        ],
      };
      invokeMock.mockResolvedValueOnce({
        profiles: [{ ...RAW_PROFILE, model: spec, effort: spec }],
        warnings: [],
      });

      const result = await invokeListProfiles();

      expect(result.profiles[0]).toMatchObject({
        model: {
          aliases: [
            { alias: 'alpha', value: 'tier-a' },
            { alias: 'beta', value: 'tier-b' },
          ],
        },
        effort: {
          aliases: [
            { alias: 'alpha', value: 'tier-a' },
            { alias: 'beta', value: 'tier-b' },
          ],
        },
      });
    });

    it.each([
      ['aliases is not an array', { aliases: 'alpha' }],
      ['an alias entry is missing alias', { aliases: [{ value: 'tier-a' }] }],
      ['an alias entry is missing value', { aliases: [{ alias: 'alpha' }] }],
      ['an alias entry has a non-string alias', { aliases: [{ alias: 1, value: 'tier-a' }] }],
      ['an alias entry has a non-string value', { aliases: [{ alias: 'alpha', value: 1 }] }],
    ])('rejects with BoundaryError when model is malformed (%s)', async (_label, malformed) => {
      invokeMock.mockResolvedValueOnce({
        profiles: [{ ...RAW_PROFILE, model: malformed }],
        warnings: [],
      });

      await expect(invokeListProfiles()).rejects.toThrow(BoundaryError);
    });
  });
});

describe('invokeSetProfileEnabled', () => {
  it.each([true, false])(
    'invokes set_profile_enabled with the profile and enabled: %s, returning the narrowed reply',
    async (enabled) => {
      invokeMock.mockResolvedValueOnce({ profile: 'codex-cli', enabled });

      const result = await invokeSetProfileEnabled('codex-cli', enabled);

      expect(invokeMock).toHaveBeenCalledWith('set_profile_enabled', {
        profile: 'codex-cli',
        enabled,
      });
      expect(result).toEqual({ profile: 'codex-cli', enabled });
    }
  );

  it('returns what the daemon answered rather than echoing the request', async () => {
    invokeMock.mockResolvedValueOnce({ profile: 'codex-cli', enabled: true });

    const result = await invokeSetProfileEnabled('codex-cli', false);

    expect(result).toEqual({ profile: 'codex-cli', enabled: true });
  });

  it.each([
    ['enabled is missing', { profile: 'codex-cli' }],
    ['enabled is a string', { profile: 'codex-cli', enabled: 'true' }],
    ['enabled is a number', { profile: 'codex-cli', enabled: 1 }],
    ['profile is missing', { enabled: true }],
    ['profile is not a string', { profile: 7, enabled: true }],
    ['the reply is not an object', null],
  ])('rejects with BoundaryError when %s', async (_label, reply) => {
    invokeMock.mockResolvedValueOnce(reply);

    await expect(invokeSetProfileEnabled('codex-cli', true)).rejects.toThrow(BoundaryError);
  });
});

describe('approval settings', () => {
  /** A well-formed reply in the core's own (snake_case) field names. */
  function approvalReply(overrides: Record<string, unknown> = {}): Record<string, unknown> {
    return {
      playbook: 'loaded',
      tools: [
        { tool: 'send_chat', auto_approve: true },
        { tool: 'close_window', auto_approve: false },
      ],
      plans_require_approval: true,
      ...overrides,
    };
  }

  const NARROWED: ApprovalSettingsInfo = {
    playbook: 'loaded',
    tools: [
      { tool: 'send_chat', autoApprove: true },
      { tool: 'close_window', autoApprove: false },
    ],
    plansRequireApproval: true,
  };

  interface ApprovalCall {
    label: string;
    call: () => Promise<ApprovalSettingsInfo>;
    command: string;
    args: Record<string, unknown> | undefined;
  }

  const CALLS: ApprovalCall[] = [
    {
      label: 'invokeGetApprovalSettings',
      call: () => invokeGetApprovalSettings(),
      command: 'get_approval_settings',
      args: undefined,
    },
    {
      label: 'invokeSetAutoApprove (on)',
      call: () => invokeSetAutoApprove('send_keys', true),
      command: 'set_auto_approve',
      // The core's commands take snake_case argument names.
      args: { tool: 'send_keys', auto_approve: true },
    },
    {
      label: 'invokeSetAutoApprove (off)',
      call: () => invokeSetAutoApprove('send_chat', false),
      command: 'set_auto_approve',
      args: { tool: 'send_chat', auto_approve: false },
    },
    {
      label: 'invokeSetPlanApprovalRequired (on)',
      call: () => invokeSetPlanApprovalRequired(true),
      command: 'set_plan_approval_required',
      args: { required: true },
    },
    {
      label: 'invokeSetPlanApprovalRequired (off)',
      call: () => invokeSetPlanApprovalRequired(false),
      command: 'set_plan_approval_required',
      args: { required: false },
    },
  ];

  describe.each(CALLS)('$label', ({ call, command, args }) => {
    it(`invokes ${command} with exactly its own arguments`, async () => {
      invokeMock.mockResolvedValueOnce(approvalReply());

      await call();

      expect(invokeMock).toHaveBeenCalledTimes(1);
      const [calledCommand, calledArgs] = invokeMock.mock.calls[0] as [string, unknown];
      expect(calledCommand).toBe(command);
      expect(calledArgs ?? {}).toEqual(args ?? {});
    });

    it('narrows the reply into camelCase', async () => {
      invokeMock.mockResolvedValueOnce(approvalReply());

      const info: ApprovalSettingsInfo = await call();

      expect(info).toEqual(NARROWED);
    });

    it('rejects with BoundaryError when the reply is not an object', async () => {
      invokeMock.mockResolvedValueOnce(null);

      await expect(call()).rejects.toThrow(BoundaryError);
    });
  });

  it('returns what the daemon answered rather than echoing the request', async () => {
    invokeMock.mockResolvedValueOnce(
      approvalReply({
        tools: [{ tool: 'send_keys', auto_approve: false }],
        plans_require_approval: true,
      })
    );

    const afterTool: ApprovalSettingsInfo = await invokeSetAutoApprove('send_keys', true);

    expect(afterTool.tools).toEqual([{ tool: 'send_keys', autoApprove: false }]);

    invokeMock.mockResolvedValueOnce(approvalReply({ plans_require_approval: true }));

    const afterPlan: ApprovalSettingsInfo = await invokeSetPlanApprovalRequired(false);

    expect(afterPlan.plansRequireApproval).toBe(true);
  });

  it('keeps the tools in the order the daemon sent them', async () => {
    const order = ['broadcast_chat', 'interrupt_window', 'send_message', 'a_newer_tool'];
    invokeMock.mockResolvedValueOnce(
      approvalReply({ tools: order.map((tool) => ({ tool, auto_approve: false })) })
    );

    const info: ApprovalSettingsInfo = await invokeGetApprovalSettings();

    expect(info.tools.map((entry) => entry.tool)).toEqual(order);
  });

  it('carries an empty tool list through', async () => {
    invokeMock.mockResolvedValueOnce(approvalReply({ tools: [] }));

    const info: ApprovalSettingsInfo = await invokeGetApprovalSettings();

    expect(info.tools).toEqual([]);
  });

  it.each(['loaded', 'not_configured', 'failed', 'unknown'] as const)(
    'keeps the playbook state %s',
    async (playbook) => {
      invokeMock.mockResolvedValueOnce(approvalReply({ playbook }));

      const info: ApprovalSettingsInfo = await invokeGetApprovalSettings();

      expect(info.playbook).toBe(playbook);
    }
  );

  it.each([
    ['a state a newer daemon added', 'read_only'],
    ['an empty string', ''],
    ['a different letter case', 'Loaded'],
    ['an inherited property name', 'toString'],
  ])('reads %s as the unknown playbook state', async (_label, playbook) => {
    invokeMock.mockResolvedValueOnce(approvalReply({ playbook }));

    const info: ApprovalSettingsInfo = await invokeGetApprovalSettings();

    expect(info.playbook).toBe('unknown');
  });

  it.each([
    ['playbook is missing', approvalReply({ playbook: undefined })],
    ['playbook is a number', approvalReply({ playbook: 1 })],
    ['playbook is null', approvalReply({ playbook: null })],
    ['tools is missing', approvalReply({ tools: undefined })],
    ['tools is not an array', approvalReply({ tools: { tool: 'send_chat', auto_approve: true } })],
    ['a tool entry is not an object', approvalReply({ tools: ['send_chat'] })],
    ['a tool entry is missing tool', approvalReply({ tools: [{ auto_approve: true }] })],
    [
      'a tool entry has a non-string tool',
      approvalReply({ tools: [{ tool: 7, auto_approve: true }] }),
    ],
    ['a tool entry is missing auto_approve', approvalReply({ tools: [{ tool: 'send_chat' }] })],
    [
      'a tool entry has a string auto_approve',
      approvalReply({ tools: [{ tool: 'send_chat', auto_approve: 'true' }] }),
    ],
    [
      'a tool entry carries the camelCase name instead of auto_approve',
      approvalReply({ tools: [{ tool: 'send_chat', autoApprove: true }] }),
    ],
    [
      'a later tool entry is malformed',
      approvalReply({
        tools: [
          { tool: 'send_chat', auto_approve: true },
          { tool: 'send_keys', auto_approve: 0 },
        ],
      }),
    ],
    ['plans_require_approval is missing', approvalReply({ plans_require_approval: undefined })],
    ['plans_require_approval is a string', approvalReply({ plans_require_approval: 'false' })],
    ['plans_require_approval is a number', approvalReply({ plans_require_approval: 0 })],
    [
      'only the daemon-config field name is present',
      approvalReply({ plans_require_approval: undefined, plan_approval_required: true }),
    ],
  ])('rejects with BoundaryError when %s', async (_label, reply) => {
    invokeMock.mockResolvedValueOnce(reply);

    await expect(invokeGetApprovalSettings()).rejects.toThrow(BoundaryError);
  });

  it('does not accept an inherited plans_require_approval', async () => {
    const inherited = Object.create({ plans_require_approval: true }) as Record<string, unknown>;
    inherited['playbook'] = 'loaded';
    inherited['tools'] = [];
    invokeMock.mockResolvedValueOnce(inherited);

    await expect(invokeGetApprovalSettings()).rejects.toThrow(BoundaryError);
  });
});

describe('subscribeSession', () => {
  it('resolves handlers per window and drops events for windows the resolver does not recognize', async () => {
    const onExitA = vi.fn();
    const onExitB = vi.fn();
    const handlersByWindow: Record<string, WindowEventHandlers> = {
      [WINDOW_A]: {
        onFrame: vi.fn(),
        onExit: onExitA,
        onDisconnected: vi.fn(),
        onSuperseded: vi.fn(),
      },
      [WINDOW_B]: {
        onFrame: vi.fn(),
        onExit: onExitB,
        onDisconnected: vi.fn(),
        onSuperseded: vi.fn(),
      },
    };
    const app: AppEventHandlers = {
      onDaemonSpawnWarning: vi.fn(),
      onBoundaryError: vi.fn(),
    };

    await subscribeSession((windowId) => handlersByWindow[windowId], app);

    const exitHandler = registeredHandlers.get(SESSION_EVENT.exit);
    expect(exitHandler).toBeDefined();

    exitHandler?.({ payload: EXIT_PAYLOAD });
    expect(onExitA).toHaveBeenCalledTimes(1);
    expect(onExitB).toHaveBeenCalledTimes(0);

    // An evicted window's delayed event is dropped silently, never thrown or
    // surfaced through onBoundaryError.
    expect(() =>
      exitHandler?.({ payload: { ...EXIT_PAYLOAD, window_id: 'evicted-window' } })
    ).not.toThrow();
    expect(onExitA).toHaveBeenCalledTimes(1);
    expect(onExitB).toHaveBeenCalledTimes(0);
    expect(app.onBoundaryError).toHaveBeenCalledTimes(0);
  });
});
