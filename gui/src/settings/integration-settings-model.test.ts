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
  integrationGroups,
  integrationsNotice,
  resultLines,
  type IntegrationGroup,
  type IntegrationRowView,
} from './integration-settings-model';
import {
  foldExternalText,
  formatCommandLine,
  formatOutputBlock,
  formatPathForDisplay,
  sanitizeForDisplay,
} from '../display/untrusted-text';
import {
  narrowMcpRegistrationsListed,
  type McpRegistrationCommand,
  type McpRegistrationEntry,
  type McpRegistrationGroup,
  type McpRegistrationResultPayload,
  type McpRegistrationRow,
  type McpRegistrationsListed,
  type McpRegistrationState,
  type McpRegistrationStatusInfo,
  type McpRegistrationStep,
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
  return {
    displayCommand: formatCommandLine(argv.slice(programAt)),
    displayArgv: formatCommandLine(argv),
  };
}

function entry(file: string, name: string, content: string): McpRegistrationEntry {
  return {
    fileKey: file,
    file: formatPathForDisplay(file),
    name: formatPathForDisplay(name),
    content: formatPathForDisplay(content),
  };
}

function group(name: string): McpRegistrationGroup {
  return { key: name, heading: sanitizeForDisplay(name) };
}

function row(profile: string, overrides: Partial<McpRegistrationRow> = {}): McpRegistrationRow {
  return {
    profile,
    displayName: sanitizeForDisplay(`${profile} name`),
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
  return { requestId: 1, bridge: bridge === null ? null : formatPathForDisplay(bridge), rows };
}

/** A listing narrowed by the boundary from rows written as the daemon sends them. */
function listedFromWire(rows: Record<string, unknown>[]): McpRegistrationsListed {
  return narrowMcpRegistrationsListed({ request_id: 1, bridge: BRIDGE, rows });
}

function rowWire(
  profile: string,
  overrides: Record<string, unknown> = {}
): Record<string, unknown> {
  return {
    profile,
    display_name: `${profile} name`,
    group: null,
    add: null,
    remove: null,
    entries: [],
    ...overrides,
  };
}

function status(
  profile: string,
  state: McpRegistrationState,
  overrides: Partial<McpRegistrationStatusInfo> = {}
): McpRegistrationStatusInfo {
  const differsIn = overrides.differsIn ?? [];
  return {
    profile,
    state,
    differsIn,
    displayDiffersIn: differsIn.map(formatPathForDisplay),
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
      row('linux-alpha', { group: group('Linux') }),
      row('alpha'),
      row('other-alpha', { group: group('Other') }),
      row('beta'),
      row('linux-beta', { group: group('Linux') }),
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
      listed([row('linux-alpha', { group: group('Linux') })]),
      statuses(),
      null
    );

    expect(groups.map((group) => group.heading)).toEqual(['Linux']);
  });

  it('keys a group named like the host heading apart from the host group', () => {
    const groups: IntegrationGroup[] = integrationGroups(
      listed([row('alpha'), row('named', { group: group(settings.integrationsHostGroup) })]),
      statuses(),
      null
    );

    expect(groups.map((group) => group.heading)).toEqual([
      text(settings.integrationsHostGroup),
      text(settings.integrationsHostGroup),
    ]);
    expect(new Set(groups.map((group) => group.key)).size).toBe(2);
  });

  it('keeps two groups apart whose names differ though they read alike', () => {
    const groups: IntegrationGroup[] = integrationGroups(
      listedFromWire([
        rowWire('joined', { group: 'a\u200Cb' }),
        rowWire('written', { group: 'a\\u{200C}b' }),
      ]),
      statuses(),
      null
    );

    expect(groups.map((group) => group.heading)).toEqual(['a\\u{200C}b', 'a\\u{200C}b']);
    expect(groups.map((group) => group.rows.map((view) => view.profile))).toEqual([
      ['joined'],
      ['written'],
    ]);
    expect(new Set(groups.map((group) => group.key)).size).toBe(2);
  });

  it('names each row by the display name the daemon sent', () => {
    const view = viewOf('registered', {}, null, {
      displayName: sanitizeForDisplay('Alpha Shown Name'),
    });

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
    const view = viewOf('unknown', {
      reason: foldExternalText('the status output could not be read'),
    });

    expect(view.primaryAction).toBe('register');
    expect(view.isPrimaryDisabled).toBe(false);
    expect(view.canRemove).toBe(true);
    expect(view.isRemoveDisabled).toBe(false);
    expect(view.reason).toBe('the status output could not be read');
  });

  it('folds the line breaks and control characters of the reason, and writes out a format character', () => {
    expect(
      viewOf('unknown', { reason: foldExternalText('status\r\nunreadable\u202E') }).reason
    ).toBe('status unreadable\\u{202E}');
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

    expect(view.stateLabel).toBe(
      text(settings.integrationDiffersIn(sanitizeForDisplay('call_timeout')))
    );
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
      file: formatPathForDisplay('.alpha/config.toml'),
      table: ['mcp_servers', 'yagura'].map(formatPathForDisplay),
      values: [{ key: formatPathForDisplay('call_timeout'), value: formatPathForDisplay('86400') }],
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

describe('integrationGroups: the notice a profile asks to show', () => {
  it("shows the profile's own sentence before and after registering, and none without one", () => {
    const notice = sanitizeForDisplay(
      'While this is registered, the CLI asks before trusting a folder.'
    );
    const states: (McpRegistrationState | null)[] = [null, 'not_registered', 'registered'];
    for (const state of states) {
      expect(viewOf(state, {}, null, { notice }).notice).toBe(notice);
    }
    expect(viewOf('not_registered').notice).toBeNull();
    expect(viewOf('registered', {}, null, { notice: null }).notice).toBeNull();
  });
});

describe('integrationsNotice', () => {
  it('says the bridge is missing when the daemon found none', () => {
    expect(integrationsNotice(listed([row('alpha')], null))).toBe(
      text(settings.integrationsNoBridge)
    );
  });

  it('has nothing to say while nothing is listed or when the bridge is known', () => {
    expect(integrationsNotice(null)).toBeNull();
    expect(integrationsNotice(listed([row('alpha')]))).toBeNull();
  });
});

describe('resultLines', () => {
  function step(
    argv: string[],
    programAt: number,
    exitCode: number | null,
    output: string
  ): McpRegistrationStep {
    return {
      displayCommand: argv.length === 0 ? null : formatCommandLine(argv.slice(programAt)),
      exitCode,
      output: formatOutputBlock(output),
    };
  }

  function result(
    overrides: Partial<McpRegistrationResultPayload> = {}
  ): McpRegistrationResultPayload {
    return {
      requestId: 3,
      profile: 'alpha',
      action: 'register',
      steps: [
        step(
          ['launcher', 'alpha', 'mcp', 'add', 'yagura', '--', 'C:\\Program Files\\y.exe'],
          1,
          0,
          'Added the server.'
        ),
        step([], 0, null, 'set call_timeout in the table'),
        step(['launcher', 'alpha', 'mcp', 'get', 'yagura'], 1, 0, ''),
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
    const lines = resultLines(result({ error: formatOutputBlock('the add step exited with 1') }));

    expect(lines.isFailure).toBe(true);
    expect(lines.text).toContain('the add step exited with 1');
    expect(lines.text).toContain('$ alpha mcp add yagura -- "C:\\Program Files\\y.exe"');

    const longOutput = 'o'.repeat(8000);
    const long = resultLines(
      result({
        steps: [step(['launcher', 'alpha', 'mcp', 'add', 'yagura'], 1, 1, longOutput)],
        error: formatOutputBlock('the add step exited with 1'),
      })
    );
    expect(long.text).toBe(
      ['$ alpha mcp add yagura', longOutput, 'the add step exited with 1'].join('\n')
    );
  });
});

describe('integrationGroups: the entries note', () => {
  it('names the single entry and the file it is added to', () => {
    const entries = [entry('.beta/config/hooks.json', 'yagura', '{"Event":[]}')];

    const view = viewOf('not_registered', {}, null, { entries });

    expect(view.entriesNote).toBe(
      text(
        settings.integrationHookEntriesNote(formatPathForDisplay('.beta/config/hooks.json'), [
          formatPathForDisplay('yagura'),
        ])
      )
    );
    for (const part of ['.beta/config/hooks.json', 'yagura']) {
      expect(view.entriesNote).toContain(part);
    }
    expect(view.entriesTitle?.join('\n')).toContain('{"Event":[]}');
  });

  it('counts several entries added to one file', () => {
    const entries = ['a', 'b', 'c'].map((name) =>
      entry('.alpha/config.toml', name, `{ hash = "${name}" }`)
    );

    const view = viewOf('not_registered', {}, null, { entries });

    expect(view.entriesNote).toBe(
      text(
        settings.integrationHookEntriesNote(
          formatPathForDisplay('.alpha/config.toml'),
          ['a', 'b', 'c'].map(formatPathForDisplay)
        )
      )
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
      settings.integrationDiffersIn(sanitizeForDisplay('args'))
    );
    expect(viewOf('differs', { differsIn: [] }).stateLabel).toBe(
      settings.integrationDiffersIn(sanitizeForDisplay(''))
    );
  });

  it('lists each hook entry on a line of its own in the title', () => {
    const entries = [entry('a.json', 'one', '1'), entry('a.json', 'two', '2')];

    expect(viewOf('not_registered', {}, null, { entries }).entriesTitle).toEqual([
      'one = 1',
      'two = 2',
    ]);
  });

  it('writes one sentence per file, in the order the files first appear, joined by a space', () => {
    const entries = [
      entry('b.json', 'one', '1'),
      entry('a.json', 'two', '2'),
      entry('b.json', 'three', '3'),
    ];
    const note = (file: string, names: string[]): string =>
      settings.integrationHookEntriesNote(
        formatPathForDisplay(file),
        names.map(formatPathForDisplay)
      );

    expect(viewOf('not_registered', {}, null, { entries }).entriesNote).toBe(
      `${note('b.json', ['one', 'three'])} ${note('a.json', ['two'])}`
    );
  });
});

/** A row whose registration runs no command and only writes into its CLI's files. */
function commandlessRow(entries: McpRegistrationRow['entries']): Partial<McpRegistrationRow> {
  return { add: null, remove: null, entries };
}

const MUSE_ENTRIES: McpRegistrationRow['entries'] = [
  entry('.config/muse/settings.json', '/mcpServers/yagura', '{}'),
  entry('.config/muse/yagura-managed-hooks.json', '/hooks', '{}'),
  entry('.config/muse/settings.json', '/managed_hooks_path', '"x.json"'),
];

describe('integrationGroups: a registration that runs no command', () => {
  it('shows each file it writes and the entries in it, in place of a command', () => {
    for (const state of ['not_registered', 'registered', 'differs'] as const) {
      const view = viewOf(state, {}, null, commandlessRow(MUSE_ENTRIES));

      expect(view.command).toBeNull();
      expect(view.commandTitle).toBeNull();
      expect(view.fileWrites).toEqual([
        {
          fileKey: '.config/muse/settings.json',
          file: '.config/muse/settings.json',
          names: ['/mcpServers/yagura', '/managed_hooks_path'],
        },
        {
          fileKey: '.config/muse/yagura-managed-hooks.json',
          file: '.config/muse/yagura-managed-hooks.json',
          names: ['/hooks'],
        },
      ]);
    }
  });

  it('keeps two files apart whose names differ though they read alike', () => {
    const listed = listedFromWire([
      rowWire('alpha', {
        entries: [
          { file: 'a\u200Cb.json', name: 'one', content: '{}' },
          { file: 'a\\u{200C}b.json', name: 'two', content: '{}' },
          { file: 'a\u200Cb.json', name: 'three', content: '{}' },
        ],
      }),
    ]);

    const view = integrationGroups(listed, statuses(status('alpha', 'not_registered')), null)[0]
      ?.rows[0];

    expect(view?.fileWrites?.map(({ file, names }) => ({ file, names }))).toEqual([
      { file: 'a\\u{200C}b.json', names: ['one', 'three'] },
      { file: 'a\\u{200C}b.json', names: ['two'] },
    ]);
  });

  it('does not also describe those entries as an addition to a command', () => {
    const view = viewOf('not_registered', {}, null, commandlessRow(MUSE_ENTRIES));

    expect(view.entriesNote).toBeNull();
  });

  it('keeps the command and its entries note for a registration that runs one', () => {
    const entries = [entry('.beta/config/hooks.json', 'yagura', '{}')];

    const view = viewOf('not_registered', {}, null, { entries });

    expect(view.fileWrites).toBeNull();
    expect(view.command).not.toBeNull();
    expect(view.entriesNote).not.toBeNull();
  });
});

describe('integrationGroups: a claim another value holds', () => {
  const CLAIM = '/managed_hooks_path';

  it('refuses Register before it is pressed and says which claim refuses it', () => {
    const view = viewOf(
      'not_registered',
      { refusedByClaim: formatPathForDisplay(CLAIM) },
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
      { differsIn: [CLAIM], refusedByClaim: formatPathForDisplay(CLAIM) },
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
