/**
 * Pure-function contract for the Models tab: which profiles offer an editor,
 * the rows it builds from a profile's current / shipped aliases and the
 * fetched choices, the edited / dirty / reset arithmetic, the request-id
 * bookkeeping a fetch races against a stale push, and the "adopt the shipped
 * list" button's condition.
 */
import { describe, it, expect } from 'vitest';

import type { LaunchOptionAlias, ModelChoice } from '../ipc/model-boundary';
import type { ProfileEntry } from '../ipc/session-client';
import { plans } from '../ui-messages';
import {
  acceptsChoicesPush,
  aliasesToSave,
  buildAliasRows,
  CHOICES_FETCH_TIMEOUT_MS,
  choicesFailureLine,
  editableProfiles,
  isChoicesFetchOverdue,
  isDirty,
  isEdited,
  nextChoicesRequestId,
  resetToShipped,
  shippedAliasesOf,
} from './model-aliases-model';

/** A well-formed `ProfileEntry`, minimal beyond the fields these functions read. */
function profileEntry(overrides: Partial<ProfileEntry> = {}): ProfileEntry {
  return {
    id: 'claude-code',
    displayName: 'Claude Code',
    detection: null,
    detectionWarning: null,
    cwd: null,
    command: [],
    available: true,
    reason: null,
    model: { aliases: [{ alias: 'fast', value: 'haiku' }] },
    effort: null,
    enabled: true,
    ...overrides,
  };
}

function alias(name: string, value: string): LaunchOptionAlias {
  return { alias: name, value };
}

function choice(id: string): ModelChoice {
  return { id, label: null };
}

/** The row for `alias`, or a thrown assertion failure when there is none. */
function requireRow<Row extends { alias: string }>(rows: readonly Row[], name: string): Row {
  const row = rows.find((entry) => entry.alias === name);
  if (row === undefined) {
    throw new Error(`expected a row for alias ${name}`);
  }
  return row;
}

describe('isEdited', () => {
  it('is false when the value matches the shipped value', () => {
    expect(isEdited('opus', 'opus')).toBe(false);
  });

  it('is true when the value differs from the shipped value', () => {
    expect(isEdited('opus2', 'opus')).toBe(true);
  });

  it('is false when there is no shipped value to compare against', () => {
    expect(isEdited('anything', null)).toBe(false);
  });
});

describe('buildAliasRows', () => {
  const current = [
    alias('fast', 'haiku'),
    alias('default', 'sonnet2'),
    alias('strong', 'custom-model'),
  ];
  const shipped = [alias('fast', 'haiku'), alias('default', 'sonnet'), alias('strong', 'opus')];
  const choices = [choice('haiku'), choice('sonnet'), choice('opus')];

  it('keeps one row per current alias, in order', () => {
    const rows = buildAliasRows(current, shipped, choices);
    expect(rows.map((row) => row.alias)).toEqual(['fast', 'default', 'strong']);
  });

  it('marks a row edited only when its value differs from the shipped value', () => {
    const rows = buildAliasRows(current, shipped, choices);
    expect(rows[0]?.edited).toBe(false);
    expect(rows[1]?.edited).toBe(true);
    expect(rows[2]?.edited).toBe(true);
  });

  it('keeps a value the fetched choices do not list, flagged rather than dropped', () => {
    const rows = buildAliasRows(current, shipped, choices);
    expect(rows[2]?.value).toBe('custom-model');
    expect(rows[2]?.valueMissingFromChoices).toBe(true);
  });

  it('does not flag a value as missing when no choices are known yet', () => {
    const rows = buildAliasRows(current, shipped, []);
    expect(rows.every((row) => row.valueMissingFromChoices === false)).toBe(true);
  });

  it('leaves every row unedited for a profile the shipped set does not cover', () => {
    const rows = buildAliasRows(current, [], choices);
    expect(rows.every((row) => row.shippedValue === null && row.edited === false)).toBe(true);
  });
});

describe('resetToShipped', () => {
  it('restores only the named row to its shipped value', () => {
    const rows = buildAliasRows(
      [alias('fast', 'sonnet'), alias('strong', 'opus')],
      [alias('fast', 'haiku'), alias('strong', 'opus')],
      []
    );
    const reset = resetToShipped(rows, 'fast');
    expect(reset.find((row) => row.alias === 'fast')?.value).toBe('haiku');
    expect(reset.find((row) => row.alias === 'strong')?.value).toBe('opus');
  });

  it('clears the edited mark of the row it restores', () => {
    const rows = buildAliasRows([alias('fast', 'sonnet')], [alias('fast', 'haiku')], []);
    const reset = resetToShipped(rows, 'fast');
    expect(reset[0]?.edited).toBe(false);
  });

  it('does not change what isDirty reports against the saved values', () => {
    const rows = buildAliasRows([alias('strong', 'temp-edit')], [alias('strong', 'opus')], []);
    const saved = [alias('strong', 'custom-saved')];
    const reset = resetToShipped(rows, 'strong');
    expect(isDirty(reset, saved)).toBe(true);
  });
});

describe('isDirty', () => {
  it('is false for rows that match the saved aliases', () => {
    const saved = [alias('fast', 'haiku')];
    const rows = buildAliasRows(saved, [alias('fast', 'haiku')], []);
    expect(isDirty(rows, saved)).toBe(false);
  });

  it('is true once a row differs from its saved value', () => {
    const saved = [alias('fast', 'haiku')];
    const rows = buildAliasRows(saved, [alias('fast', 'haiku')], []);
    const edited = [{ ...requireRow(rows, 'fast'), value: 'sonnet' }];
    expect(isDirty(edited, saved)).toBe(true);
  });

  it('is false again once the value is put back to what was saved', () => {
    const saved = [alias('fast', 'haiku')];
    const rows = buildAliasRows(saved, [alias('fast', 'haiku')], []);
    const edited = [{ ...requireRow(rows, 'fast'), value: 'sonnet' }];
    const reverted = [{ ...requireRow(edited, 'fast'), value: 'haiku' }];
    expect(isDirty(reverted, saved)).toBe(false);
  });
});

describe('aliasesToSave', () => {
  it('carries only the alias and its current value, in row order', () => {
    const rows = buildAliasRows(
      [alias('fast', 'haiku'), alias('strong', 'custom')],
      [alias('fast', 'haiku'), alias('strong', 'opus')],
      []
    );
    expect(aliasesToSave(rows)).toEqual([alias('fast', 'haiku'), alias('strong', 'custom')]);
  });
});

describe('editableProfiles', () => {
  it('excludes a profile that maps no model axis', () => {
    const profiles = [profileEntry({ id: 'shell', model: null })];
    expect(editableProfiles(profiles)).toHaveLength(0);
  });

  it('excludes a profile whose model axis declares no aliases', () => {
    const profiles = [profileEntry({ id: 'empty', model: { aliases: [] } })];
    expect(editableProfiles(profiles)).toHaveLength(0);
  });

  it('includes a profile with at least one alias', () => {
    const profiles = [profileEntry({ id: 'claude-code' })];
    expect(editableProfiles(profiles).map((p) => p.id)).toEqual(['claude-code']);
  });
});

describe('nextChoicesRequestId', () => {
  it('returns a different id on successive calls', () => {
    const first = nextChoicesRequestId(0);
    const second = nextChoicesRequestId(first);
    expect(second).not.toBe(first);
  });
});

describe('acceptsChoicesPush', () => {
  it('accepts a push whose request id matches the latest issued for its profile', () => {
    const latest = new Map([['claude-code', 9]]);
    expect(acceptsChoicesPush(latest, { profile: 'claude-code', request_id: 9 })).toBe(true);
  });

  it('rejects a push carrying a stale request id', () => {
    const latest = new Map([['claude-code', 9]]);
    expect(acceptsChoicesPush(latest, { profile: 'claude-code', request_id: 8 })).toBe(false);
  });

  it('takes a push that arrives before the immediate acknowledgement, as long as the id was already recorded', () => {
    // The id is recorded at issue time, before `invoke` resolves, so a push
    // racing ahead of the acknowledgement still matches.
    const latest = new Map([['claude-code', 9]]);
    expect(acceptsChoicesPush(latest, { profile: 'claude-code', request_id: 9 })).toBe(true);
  });

  it('keeps two profiles from influencing each other', () => {
    const latest = new Map([
      ['claude-code', 9],
      ['codex-cli', 41],
    ]);
    expect(acceptsChoicesPush(latest, { profile: 'codex-cli', request_id: 9 })).toBe(false);
    expect(acceptsChoicesPush(latest, { profile: 'codex-cli', request_id: 41 })).toBe(true);
  });
});

describe('isChoicesFetchOverdue', () => {
  it('is longer than the daemon default listing timeout of 20 seconds', () => {
    expect(CHOICES_FETCH_TIMEOUT_MS).toBeGreaterThan(20_000);
  });

  it('is false just under the timeout', () => {
    expect(isChoicesFetchOverdue(1_000, 1_000 + CHOICES_FETCH_TIMEOUT_MS - 1)).toBe(false);
  });

  it('is true once the timeout has fully elapsed', () => {
    expect(isChoicesFetchOverdue(1_000, 1_000 + CHOICES_FETCH_TIMEOUT_MS)).toBe(true);
  });
});

describe('shippedAliasesOf', () => {
  it('takes the shipped value of each model alias, leaving out one the shipped profile lacks', () => {
    const aliases = shippedAliasesOf({
      profile: 'codex-cli',
      shippedBase: true,
      command: { argv: ['codex'], shipped: ['codex'], changed: false },
      entries: [
        { path: ['model', 'args'], value: ['-m'], shipped: ['-m'], changed: false },
        { path: ['model', 'aliases', 'fast'], value: 'a', shipped: 'a', changed: false },
        { path: ['model', 'aliases', 'strong'], value: 'mine', shipped: 'b', changed: true },
        { path: ['model', 'aliases', 'extra'], value: 'x', shipped: undefined, changed: true },
        { path: ['effort', 'aliases', 'fast'], value: 'low', shipped: 'low', changed: false },
      ],
    });
    expect(aliases).toEqual([
      { alias: 'fast', value: 'a' },
      { alias: 'strong', value: 'b' },
    ]);
  });
});

describe('model list edge cases', () => {
  it('counts a profile with a model axis of one alias as editable, and leaves the order alone', () => {
    const none = profileEntry({ id: 'none', model: null });
    const empty = profileEntry({ id: 'empty', model: { aliases: [] } });
    const one = profileEntry({ id: 'one' });
    const two = profileEntry({ id: 'two' });

    expect(editableProfiles([none, one, empty, two]).map((profile) => profile.id)).toEqual([
      'one',
      'two',
    ]);
  });

  it('gives an alias the shipped profile lacks no shipped value and no edited mark', () => {
    const rows = buildAliasRows([alias('mine', 'x')], [alias('other', 'y')], []);

    expect(rows).toEqual([
      {
        alias: 'mine',
        value: 'x',
        shippedValue: null,
        edited: false,
        valueMissingFromChoices: false,
      },
    ]);
  });

  it('flags a value missing from a known list, and only that one', () => {
    const rows = buildAliasRows(
      [alias('a', 'listed'), alias('b', 'absent')],
      [],
      [choice('listed')]
    );

    expect(rows.map((row) => row.valueMissingFromChoices)).toEqual([false, true]);
  });

  it('does not flag a value that is one of several listed choices', () => {
    const rows = buildAliasRows([alias('a', 'second')], [], [choice('first'), choice('second')]);

    expect(rows[0]?.valueMissingFromChoices).toBe(false);
  });

  it('judges a restored row against the list it is given, and defaults to no list', () => {
    const rows = buildAliasRows([alias('a', 'mine')], [alias('a', 'shipped')], []);

    expect(resetToShipped(rows, 'a')[0]?.valueMissingFromChoices).toBe(false);
    expect(resetToShipped(rows, 'a', [choice('other')])[0]?.valueMissingFromChoices).toBe(true);
    expect(resetToShipped(rows, 'a', [choice('shipped')])[0]?.valueMissingFromChoices).toBe(false);
  });

  it('leaves a row with no shipped value as it is when asked to reset it', () => {
    const rows = buildAliasRows([alias('a', 'mine')], [], []);

    expect(resetToShipped(rows, 'a')).toEqual(rows);
  });

  it('leaves the other rows as they are when one is reset', () => {
    const rows = buildAliasRows(
      [alias('a', 'mine'), alias('b', 'mine')],
      [alias('a', 'sa'), alias('b', 'sb')],
      []
    );

    const reset = resetToShipped(rows, 'a');

    expect(reset.map((row) => row.value)).toEqual(['sa', 'mine']);
    expect(reset[1]).toBe(rows[1]);
  });

  it('is dirty when the number of rows differs from the saved ones, in either direction', () => {
    const rows = buildAliasRows([alias('a', 'x')], [], []);

    expect(isDirty(rows, [])).toBe(true);
    expect(isDirty([], [alias('a', 'x')])).toBe(true);
  });

  it('is dirty when a row has an alias that was not saved, even with the same count', () => {
    const rows = buildAliasRows([alias('new', 'x')], [], []);

    expect(isDirty(rows, [alias('old', 'x')])).toBe(true);
  });

  it('is not dirty with no rows and nothing saved', () => {
    expect(isDirty([], [])).toBe(false);
  });
});

describe('nextChoicesRequestId range', () => {
  it.each<[number, number]>([
    [0, 1],
    [41, 42],
    [Number.MAX_SAFE_INTEGER - 1, Number.MAX_SAFE_INTEGER],
    [Number.MAX_SAFE_INTEGER, 0],
    [-1, 0],
    [-5, 0],
    [1.5, 0],
    [Number.NaN, 0],
    [Number.POSITIVE_INFINITY, 0],
  ])('follows %s with %s', (given, next) => {
    expect(nextChoicesRequestId(given)).toBe(next);
  });
});

describe('isChoicesFetchOverdue boundary', () => {
  it('is overdue exactly when the full time has elapsed', () => {
    expect(isChoicesFetchOverdue(1000, 1000 + CHOICES_FETCH_TIMEOUT_MS - 1)).toBe(false);
    expect(isChoicesFetchOverdue(1000, 1000 + CHOICES_FETCH_TIMEOUT_MS)).toBe(true);
    expect(isChoicesFetchOverdue(1000, 1000 + CHOICES_FETCH_TIMEOUT_MS + 1)).toBe(true);
  });
});

describe('choicesFailureLine', () => {
  it('shows the reason that was reported', () => {
    expect(choicesFailureLine({ kind: 'reported', reason: 'cli missing' })).toBe(
      plans.modelChoicesFailed('cli missing')
    );
  });

  it('says the request timed out for an overdue list', () => {
    expect(choicesFailureLine({ kind: 'overdue' })).toBe(
      plans.modelChoicesFailed(plans.modelChoicesOverdueReason)
    );
    expect(choicesFailureLine({ kind: 'overdue' })).not.toBe(
      choicesFailureLine({ kind: 'reported', reason: 'cli missing' })
    );
  });
});

describe('shippedAliasesOf reads only model aliases with a string shipped value', () => {
  function detailOf(
    entries: Array<{ path: string[]; shipped: unknown }>
  ): Parameters<typeof shippedAliasesOf>[0] {
    return {
      profile: 'p',
      shippedBase: true,
      command: { argv: ['p'], shipped: ['p'], changed: false },
      entries: entries.map(({ path, shipped }) => ({ path, value: 'v', shipped, changed: false })),
    };
  }

  it('ignores a longer path, a shorter path, another table, another key and a non-string value', () => {
    const aliases = shippedAliasesOf(
      detailOf([
        { path: ['model', 'aliases', 'deep', 'er'], shipped: 'a' },
        { path: ['model', 'aliases'], shipped: 'a' },
        { path: ['other', 'aliases', 'a'], shipped: 'a' },
        { path: ['model', 'args', 'a'], shipped: 'a' },
        { path: ['model', 'aliases', 'num'], shipped: 3 },
        { path: ['model', 'aliases', 'null'], shipped: null },
        { path: ['model', 'aliases', 'ok'], shipped: 'kept' },
      ])
    );

    expect(aliases).toEqual([{ alias: 'ok', value: 'kept' }]);
  });
});
