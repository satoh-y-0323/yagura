/**
 * The pure model behind the Settings panel's Integrations tab: how the rows
 * the daemon listed are grouped, which buttons each registration state
 * offers, the command each row shows, the one line explaining why the tab
 * cannot act, and the text an operation's result leaves behind.
 *
 * Expected wording is read from the i18n resource, never copied here, so these
 * tests pin which message each state picks rather than the message itself.
 */
import { describe, it, expect } from 'vitest';

import {
  displayCommand,
  integrationGroups,
  integrationsNotice,
  isIntegrationsSupported,
  resultLines,
  type IntegrationGroup,
  type IntegrationRowView,
} from './integration-settings-model';
import { MCP_REGISTRATION_MIN_REVISION } from '../ipc/session-boundary';
import type {
  McpRegistrationCommand,
  McpRegistrationResultPayload,
  McpRegistrationRow,
  McpRegistrationsListed,
  McpRegistrationState,
  McpRegistrationStatusInfo,
} from '../ipc/mcp-registration-boundary';
import { settings } from '../ui-messages';

const BRIDGE = 'C:\\bridge\\yagura-mcp.exe';

/**
 * Reads one message from the resource and fails unless it is real text, so a
 * key that does not exist can never compare equal to a missing value.
 */
function text(value: unknown): string {
  expect(typeof value).toBe('string');
  expect((value as string).length).toBeGreaterThan(0);
  return value as string;
}

function command(argv: string[], programAt = 0): McpRegistrationCommand {
  return { argv, programAt };
}

function row(profile: string, overrides: Partial<McpRegistrationRow> = {}): McpRegistrationRow {
  return {
    profile,
    displayName: `${profile} name`,
    group: null,
    add: command(['launcher', profile, 'mcp', 'add', 'yagura', '--', BRIDGE], 1),
    remove: command(['launcher', profile, 'mcp', 'remove', 'yagura'], 1),
    tomlSet: null,
    entries: [],
    ...overrides,
  };
}

function listed(
  rows: McpRegistrationRow[],
  bridge: string | null = BRIDGE
): McpRegistrationsListed {
  return { requestId: 1, bridge, rows };
}

function status(
  profile: string,
  state: McpRegistrationState,
  overrides: Partial<McpRegistrationStatusInfo> = {}
): McpRegistrationStatusInfo {
  return {
    profile,
    state,
    differsIn: [],
    reason: null,
    add: null,
    remove: null,
    refusedByClaim: null,
    ...overrides,
  };
}

function statuses(
  ...infos: McpRegistrationStatusInfo[]
): ReadonlyMap<string, McpRegistrationStatusInfo> {
  return new Map(infos.map((info) => [info.profile, info]));
}

/** The single row `integrationGroups` builds for one listed row in one state. */
function viewOf(
  state: McpRegistrationState | null,
  overrides: Partial<McpRegistrationStatusInfo> = {},
  running: string | null = null,
  rowOverrides: Partial<McpRegistrationRow> = {}
): IntegrationRowView {
  const groups: IntegrationGroup[] = integrationGroups(
    listed([row('alpha', rowOverrides)]),
    state === null ? statuses() : statuses(status('alpha', state, overrides)),
    running
  );
  const view = groups[0]?.rows[0];
  expect(view).toBeDefined();
  return view as IntegrationRowView;
}

/** Whether the row offers no button the user can press. */
function hasNoEnabledButton(view: IntegrationRowView): boolean {
  const primaryOff = view.primaryAction === null || view.isPrimaryDisabled;
  const removeOff = !view.canRemove || view.isRemoveDisabled;
  return primaryOff && removeOff;
}

describe('integrationGroups: grouping', () => {
  it('puts the rows without a group first under the host heading, then each group in row order', () => {
    const rows = [
      row('linux-alpha', { group: 'Linux' }),
      row('alpha'),
      row('other-alpha', { group: 'Other' }),
      row('beta'),
      row('linux-beta', { group: 'Linux' }),
    ];

    const groups: IntegrationGroup[] = integrationGroups(listed(rows), statuses(), null);

    expect(groups.map((group) => group.heading)).toEqual([
      text(settings.integrationsHostGroup),
      'Linux',
      'Other',
    ]);
    expect(groups.map((group) => group.rows.map((view) => view.profile))).toEqual([
      ['alpha', 'beta'],
      ['linux-alpha', 'linux-beta'],
      ['other-alpha'],
    ]);
  });

  it('shows no host heading when every row belongs to a group', () => {
    const groups: IntegrationGroup[] = integrationGroups(
      listed([row('linux-alpha', { group: 'Linux' })]),
      statuses(),
      null
    );

    expect(groups.map((group) => group.heading)).toEqual(['Linux']);
  });

  it('names each row by the display name the daemon sent', () => {
    const view = viewOf('registered', {}, null, { displayName: 'Alpha Shown Name' });

    expect(view.name).toBe('Alpha Shown Name');
    expect(view.profile).toBe('alpha');
  });
});

describe('integrationGroups: buttons per state', () => {
  it('offers only Remove for a registered row', () => {
    const view = viewOf('registered');

    expect(view.primaryAction).toBeNull();
    expect(view.canRemove).toBe(true);
    expect(view.isRemoveDisabled).toBe(false);
  });

  it('offers Update and Remove for a row registered differently', () => {
    const view = viewOf('differs', { differsIn: ['command'] });

    expect(view.primaryAction).toBe('update');
    expect(view.isPrimaryDisabled).toBe(false);
    expect(view.canRemove).toBe(true);
    expect(view.isRemoveDisabled).toBe(false);
  });

  it('offers only Register for a row that is not registered', () => {
    const view = viewOf('not_registered');

    expect(view.primaryAction).toBe('register');
    expect(view.isPrimaryDisabled).toBe(false);
    expect(view.canRemove).toBe(false);
  });

  it('offers a Register that cannot be pressed for a CLI that is not installed', () => {
    const view = viewOf('not_installed');

    expect(view.primaryAction).toBe('register');
    expect(view.isPrimaryDisabled).toBe(true);
    expect(view.canRemove).toBe(false);
  });

  it('offers Register and Remove with the reason for a row whose state could not be read', () => {
    const view = viewOf('unknown', { reason: 'the status output could not be read' });

    expect(view.primaryAction).toBe('register');
    expect(view.isPrimaryDisabled).toBe(false);
    expect(view.canRemove).toBe(true);
    expect(view.isRemoveDisabled).toBe(false);
    expect(view.reason).toBe('the status output could not be read');
  });

  it('carries no reason for a row whose state was read', () => {
    expect(viewOf('registered').reason).toBeNull();
  });

  it('offers nothing to press while the state has not arrived', () => {
    const view = viewOf(null);

    expect(hasNoEnabledButton(view)).toBe(true);
    expect(view.stateLabel).toBe(text(settings.integrationChecking));
  });

  it('freezes every changing button on every row while one operation runs', () => {
    const groups: IntegrationGroup[] = integrationGroups(
      listed([row('alpha'), row('beta')]),
      statuses(status('alpha', 'differs', { differsIn: ['command'] }), status('beta', 'unknown')),
      'alpha'
    );
    const views = groups.flatMap((group) => group.rows);

    expect(views).toHaveLength(2);
    for (const view of views) {
      expect(view.isPrimaryDisabled).toBe(true);
      expect(view.isRemoveDisabled).toBe(true);
    }
  });

  it('leaves the same buttons pressable once nothing runs', () => {
    const groups: IntegrationGroup[] = integrationGroups(
      listed([row('alpha'), row('beta')]),
      statuses(status('alpha', 'differs', { differsIn: ['command'] }), status('beta', 'unknown')),
      null
    );
    const views = groups.flatMap((group) => group.rows);

    expect(views).toHaveLength(2);
    for (const view of views) {
      expect(view.isPrimaryDisabled).toBe(false);
      expect(view.isRemoveDisabled).toBe(false);
    }
  });
});

describe('integrationGroups: state wording and tone', () => {
  it.each([
    ['registered', 'integrationRegistered', 'ok'],
    ['not_registered', 'integrationNotRegistered', 'none'],
    ['not_installed', 'integrationNotInstalled', 'none'],
    ['unknown', 'integrationUnknown', 'warn'],
  ] as const)('labels %s with settings.%s in the %s tone', (state, key, tone) => {
    const view = viewOf(state);

    expect(view.stateLabel).toBe(text(settings[key]));
    expect(view.stateTone).toBe(tone);
  });

  it('says the row is registered to a different bridge when the command differs', () => {
    const view = viewOf('differs', { differsIn: ['command'] });

    expect(view.stateLabel).toBe(text(settings.integrationDiffers));
    expect(view.stateTone).toBe('warn');
  });

  it('keeps the bridge wording when the command differs together with another key', () => {
    const view = viewOf('differs', { differsIn: ['call_timeout', 'command'] });

    expect(view.stateLabel).toBe(text(settings.integrationDiffers));
  });

  it('names the key when only another key differs', () => {
    const view = viewOf('differs', { differsIn: ['call_timeout'] });

    expect(view.stateLabel).toBe(text(settings.integrationDiffersIn('call_timeout')));
    expect(view.stateLabel).toContain('call_timeout');
    expect(view.stateLabel).not.toBe(text(settings.integrationDiffers));
    expect(view.stateTone).toBe('warn');
  });
});

describe('integrationGroups: the command a row shows', () => {
  it('shows the add command from the CLI on for a row that is not registered', () => {
    const view = viewOf('not_registered');

    expect(view.command).toBe(`alpha mcp add yagura -- ${BRIDGE}`);
    expect(view.commandTitle).toBe(`launcher alpha mcp add yagura -- ${BRIDGE}`);
  });

  it('shows the remove command for a registered row', () => {
    const view = viewOf('registered');

    expect(view.command).toBe('alpha mcp remove yagura');
    expect(view.commandTitle).toBe('launcher alpha mcp remove yagura');
  });

  it('prefers the settled command the status push carries over the listed one', () => {
    const settled = command(
      ['launcher', 'alpha', 'mcp', 'add', 'yagura', '--', '/mnt/c/bridge'],
      1
    );

    const view = viewOf('not_registered', { add: settled });

    expect(view.command).toBe('alpha mcp add yagura -- /mnt/c/bridge');
    expect(view.commandTitle).toBe('launcher alpha mcp add yagura -- /mnt/c/bridge');
  });
});

describe('integrationGroups: the settings-file note', () => {
  it('describes the change from the file, table and values the daemon sent', () => {
    const tomlSet = {
      file: '.alpha/config.toml',
      table: ['mcp_servers', 'yagura'],
      values: [{ key: 'call_timeout', value: '86400' }],
    };

    const view = viewOf('not_registered', {}, null, { tomlSet });

    expect(view.tomlSetNote).toBe(
      text(settings.integrationTomlSetNote(tomlSet.file, tomlSet.table, tomlSet.values))
    );
    for (const part of ['.alpha/config.toml', 'mcp_servers', 'yagura', 'call_timeout', '86400']) {
      expect(view.tomlSetNote).toContain(part);
    }
  });

  it('has no note for a row without a settings-file change', () => {
    expect(viewOf('not_registered').tomlSetNote).toBeNull();
  });
});

describe('displayCommand', () => {
  it('shows the argv from the CLI on, joined by spaces', () => {
    expect(displayCommand(command(['wrap', '-x', 'alpha', 'mcp', 'remove', 'yagura'], 2))).toBe(
      'alpha mcp remove yagura'
    );
  });

  it('quotes an element that contains whitespace', () => {
    expect(
      displayCommand(command(['alpha', 'mcp', 'add', 'yagura', '--', 'C:\\Program Files\\y.exe']))
    ).toBe('alpha mcp add yagura -- "C:\\Program Files\\y.exe"');
  });
});

describe('isIntegrationsSupported', () => {
  it('needs the revision that carries MCP registration', () => {
    expect(MCP_REGISTRATION_MIN_REVISION).toBe(13);
  });

  it.each([
    [null, true],
    [12, false],
    [13, true],
  ] as const)('reads revision %s as supported: %s', (revision, supported) => {
    expect(isIntegrationsSupported(revision)).toBe(supported);
  });
});

describe('integrationsNotice', () => {
  it('asks for a newer daemon when the revision is too old, before anything else', () => {
    expect(integrationsNotice(listed([row('alpha')], null), 12)).toBe(
      text(settings.integrationsNeedNewerDaemon)
    );
    expect(integrationsNotice(null, 12)).toBe(text(settings.integrationsNeedNewerDaemon));
  });

  it('says the bridge is missing when the daemon found none', () => {
    expect(integrationsNotice(listed([row('alpha')], null), 13)).toBe(
      text(settings.integrationsNoBridge)
    );
  });

  it('has nothing to say while nothing is listed or when the bridge is known', () => {
    expect(integrationsNotice(null, 13)).toBeNull();
    expect(integrationsNotice(listed([row('alpha')]), null)).toBeNull();
  });
});

describe('resultLines', () => {
  function result(
    overrides: Partial<McpRegistrationResultPayload> = {}
  ): McpRegistrationResultPayload {
    return {
      requestId: 3,
      profile: 'alpha',
      action: 'register',
      steps: [
        {
          argv: ['launcher', 'alpha', 'mcp', 'add', 'yagura', '--', 'C:\\Program Files\\y.exe'],
          programAt: 1,
          exitCode: 0,
          output: 'Added the server.',
        },
        { argv: [], programAt: 0, exitCode: null, output: 'set call_timeout in the table' },
        {
          argv: ['launcher', 'alpha', 'mcp', 'get', 'yagura'],
          programAt: 1,
          exitCode: 0,
          output: '',
        },
      ],
      error: null,
      status: status('alpha', 'registered'),
      ...overrides,
    };
  }

  it('shows each step as its displayed command followed by its output', () => {
    const lines = resultLines(result());

    expect(lines.text).toBe(
      [
        '$ alpha mcp add yagura -- "C:\\Program Files\\y.exe"',
        'Added the server.',
        'set call_timeout in the table',
        '$ alpha mcp get yagura',
      ].join('\n')
    );
    expect(lines.isFailure).toBe(false);
  });

  it('marks the result as a failure and shows the error when there is one', () => {
    const lines = resultLines(result({ error: 'the add step exited with 1' }));

    expect(lines.isFailure).toBe(true);
    expect(lines.text).toContain('the add step exited with 1');
    expect(lines.text).toContain('$ alpha mcp add yagura -- "C:\\Program Files\\y.exe"');
  });
});

describe('integrationGroups: the entries note', () => {
  it('names the single entry and the file it is added to', () => {
    const entries = [{ file: '.beta/config/hooks.json', name: 'yagura', content: '{"Event":[]}' }];

    const view = viewOf('not_registered', {}, null, { entries });

    expect(view.entriesNote).toBe(
      text(settings.integrationHookEntriesNote('.beta/config/hooks.json', ['yagura']))
    );
    for (const part of ['.beta/config/hooks.json', 'yagura']) {
      expect(view.entriesNote).toContain(part);
    }
    expect(view.entriesTitle).toContain('{"Event":[]}');
  });

  it('counts several entries added to one file', () => {
    const entries = ['a', 'b', 'c'].map((name) => ({
      file: '.alpha/config.toml',
      name,
      content: `{ hash = "${name}" }`,
    }));

    const view = viewOf('not_registered', {}, null, { entries });

    expect(view.entriesNote).toBe(
      text(settings.integrationHookEntriesNote('.alpha/config.toml', ['a', 'b', 'c']))
    );
    for (const name of ['a', 'b', 'c']) {
      expect(view.entriesTitle).toContain(`${name} = { hash = "${name}" }`);
    }
  });

  it('has no note for a row without entries', () => {
    const view = viewOf('not_registered');

    expect(view.entriesNote).toBeNull();
    expect(view.entriesTitle).toBeNull();
  });
});

describe('integrationGroups: details of the state and the notes', () => {
  it('shows no color while the state has not arrived', () => {
    expect(viewOf(null).stateTone).toBe('none');
  });

  it('shows no color for a row that is not registered or whose CLI is not installed', () => {
    expect(viewOf('not_registered').stateTone).toBe('none');
    expect(viewOf('not_installed').stateTone).toBe('none');
  });

  it('colors a registered row ok and an unreadable or differing row warn', () => {
    expect(viewOf('registered').stateTone).toBe('ok');
    expect(viewOf('unknown').stateTone).toBe('warn');
    expect(viewOf('differs', { differsIn: ['command'] }).stateTone).toBe('warn');
    expect(viewOf('differs', { differsIn: ['args'] }).stateTone).toBe('warn');
  });

  it('names the key that differs, or an empty one when the daemon named none', () => {
    expect(viewOf('differs', { differsIn: ['args'] }).stateLabel).toBe(
      settings.integrationDiffersIn('args')
    );
    expect(viewOf('differs', { differsIn: [] }).stateLabel).toBe(settings.integrationDiffersIn(''));
  });

  it('lists each hook entry on a line of its own in the title', () => {
    const entries = [
      { file: 'a.json', name: 'one', content: '1' },
      { file: 'a.json', name: 'two', content: '2' },
    ];

    expect(viewOf('not_registered', {}, null, { entries }).entriesTitle).toBe('one = 1\ntwo = 2');
  });

  it('writes one sentence per file, in the order the files first appear, joined by a space', () => {
    const entries = [
      { file: 'b.json', name: 'one', content: '1' },
      { file: 'a.json', name: 'two', content: '2' },
      { file: 'b.json', name: 'three', content: '3' },
    ];

    expect(viewOf('not_registered', {}, null, { entries }).entriesNote).toBe(
      `${settings.integrationHookEntriesNote('b.json', ['one', 'three'])} ${settings.integrationHookEntriesNote('a.json', ['two'])}`
    );
  });
});

/** A row whose registration runs no command and only writes into its CLI's files. */
function commandlessRow(entries: McpRegistrationRow['entries']): Partial<McpRegistrationRow> {
  return { add: null, remove: null, entries };
}

const MUSE_ENTRIES: McpRegistrationRow['entries'] = [
  { file: '.config/muse/settings.json', name: '/mcpServers/yagura', content: '{}' },
  { file: '.config/muse/yagura-managed-hooks.json', name: '/hooks', content: '{}' },
  { file: '.config/muse/settings.json', name: '/managed_hooks_path', content: '"x.json"' },
];

describe('integrationGroups: a registration that runs no command', () => {
  it('shows each file it writes and the entries in it, in place of a command', () => {
    for (const state of ['not_registered', 'registered', 'differs'] as const) {
      const view = viewOf(state, {}, null, commandlessRow(MUSE_ENTRIES));

      expect(view.command).toBe('');
      expect(view.commandTitle).toBe('');
      expect(view.fileWrites).toEqual([
        {
          file: '.config/muse/settings.json',
          names: ['/mcpServers/yagura', '/managed_hooks_path'],
        },
        { file: '.config/muse/yagura-managed-hooks.json', names: ['/hooks'] },
      ]);
    }
  });

  it('does not also describe those entries as an addition to a command', () => {
    const view = viewOf('not_registered', {}, null, commandlessRow(MUSE_ENTRIES));

    expect(view.entriesNote).toBeNull();
  });

  it('keeps the command and its entries note for a registration that runs one', () => {
    const entries = [{ file: '.beta/config/hooks.json', name: 'yagura', content: '{}' }];

    const view = viewOf('not_registered', {}, null, { entries });

    expect(view.fileWrites).toBeNull();
    expect(view.command).not.toBe('');
    expect(view.entriesNote).not.toBeNull();
  });
});

describe('integrationGroups: a claim another value holds', () => {
  const CLAIM = '/managed_hooks_path';

  it('refuses Register before it is pressed and says which claim refuses it', () => {
    const view = viewOf(
      'not_registered',
      { refusedByClaim: CLAIM },
      null,
      commandlessRow(MUSE_ENTRIES)
    );

    expect(view.primaryAction).toBe('register');
    expect(view.isPrimaryDisabled).toBe(true);
    expect(view.refusal).toContain('managed_hooks_path');
  });

  it('refuses Update but leaves Remove pressable, since only Register and Update are refused', () => {
    const view = viewOf(
      'differs',
      { differsIn: [CLAIM], refusedByClaim: CLAIM },
      null,
      commandlessRow(MUSE_ENTRIES)
    );

    expect(view.primaryAction).toBe('update');
    expect(view.isPrimaryDisabled).toBe(true);
    expect(view.canRemove).toBe(true);
    expect(view.isRemoveDisabled).toBe(false);
    expect(view.refusal).toContain('managed_hooks_path');
  });

  it('refuses nothing and says nothing when no claim refuses', () => {
    const view = viewOf('not_registered', {}, null, commandlessRow(MUSE_ENTRIES));

    expect(view.isPrimaryDisabled).toBe(false);
    expect(view.refusal).toBeNull();
  });
});
