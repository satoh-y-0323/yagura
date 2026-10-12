import { describe, expect, it } from 'vitest';

import type { ProfileDetail, ProfileDetailEntry } from '../ipc/profile-detail-boundary';
import {
  canSaveCommand,
  detailSections,
  isCommandDirty,
  platformName,
  valueText,
} from './profile-detail-model';

function entry(path: string[], value: unknown, overrides: Partial<ProfileDetailEntry> = {}) {
  return { path, value, shipped: value, changed: false, ...overrides };
}

function detail(entries: ProfileDetailEntry[]): ProfileDetail {
  return {
    profile: 'codex-cli',
    shippedBase: true,
    command: { argv: ['codex'], shipped: ['codex'], changed: false },
    entries,
  };
}

describe('detailSections', () => {
  it('sorts values into their sections and leaves out what the row shows elsewhere', () => {
    const sections = detailSections(
      detail([
        entry(['schema'], 1),
        entry(['enabled'], true),
        entry(['display_name'], 'Codex CLI'),
        entry(['command', 'default'], ['codex']),
        entry(['model', 'args'], ['-m', '{value}']),
        entry(['model', 'aliases', 'strong'], 'my-model', {
          shipped: 'gpt-6-astra',
          changed: true,
        }),
        entry(['env', 'TOKEN'], 'x', { shipped: undefined, changed: true }),
      ])
    );

    expect(sections.launch.map((row) => [row.key, row.text])).toEqual([
      ['display_name', 'Codex CLI'],
      ['model.args', '-m {value}'],
    ]);
    expect(sections.aliases).toEqual([
      { key: 'model strong', text: 'my-model', shippedText: 'gpt-6-astra', changed: true },
    ]);
    expect(sections.environment).toEqual([
      { key: 'TOKEN', text: 'x', shippedText: null, changed: true },
    ]);
  });

  it('shows a provided table as one line, changed when any value under it is', () => {
    const sections = detailSections(
      detail([
        entry(['mcp', 'args'], ['-c', 'x']),
        entry(['hooks', 'args'], ['-c', 'y']),
        entry(['hooks', 'config_file'], 'z', { changed: true }),
        entry(['model', 'choices', 'args'], ['debug', 'models']),
      ])
    );

    expect(sections.provided.map((row) => [row.key, row.changed])).toEqual([
      ['mcp', false],
      ['hooks', true],
      ['model.choices', false],
    ]);
    expect(sections.launch).toEqual([]);
  });

  it('gives no shipped text to a value the user left alone', () => {
    const sections = detailSections(detail([entry(['detection'], 'codex-cli')]));
    expect(sections.launch[0]?.shippedText).toBeNull();
  });
});

describe('valueText', () => {
  it('joins a list of strings and writes anything else as JSON', () => {
    expect(valueText(['-m', '{value}'])).toBe('-m {value}');
    expect(valueText(86400)).toBe('86400');
    expect(valueText([{ a: 1 }])).toBe('[{"a":1}]');
  });
});

describe('canSaveCommand', () => {
  it('needs a change, at least one argument and no empty argument', () => {
    expect(canSaveCommand(['codex'], ['codex'])).toBe(false);
    expect(canSaveCommand(['codex', '--x'], ['codex'])).toBe(true);
    expect(canSaveCommand([], ['codex'])).toBe(false);
    expect(canSaveCommand(['codex', ''], ['codex'])).toBe(false);
  });
});

describe('valueText forms', () => {
  it.each<[string, unknown, string]>([
    ['a string', 'codex', 'codex'],
    ['an empty list', [], ''],
    ['a list of strings', ['a', 'b', 'c'], 'a b c'],
    ['a list that is not all strings', ['a', 1], '["a",1]'],
    ['a number', 0, '0'],
    ['true', true, 'true'],
    ['false', false, 'false'],
    ['null', null, 'null'],
    ['a table', { a: 1 }, '{"a":1}'],
    ['a missing value', undefined, ''],
  ])('writes %s as one line', (_label, value, text) => {
    expect(valueText(value)).toBe(text);
  });
});

describe('detailSections rows', () => {
  it('shows the shipped value only for a changed value that has one', () => {
    const sections = detailSections(
      detail([
        entry(['a'], 'mine', { shipped: 'theirs', changed: true }),
        entry(['b'], 'mine', { shipped: 'theirs', changed: false }),
        entry(['c'], 'mine', { shipped: undefined, changed: true }),
        entry(['d'], 'mine', { shipped: null, changed: true }),
        entry(['e'], 'mine', { shipped: ['x', 'y'], changed: true }),
      ])
    );

    expect(sections.launch.map((row) => [row.key, row.shippedText])).toEqual([
      ['a', 'theirs'],
      ['b', null],
      ['c', null],
      ['d', null],
      ['e', 'x y'],
    ]);
  });

  it('keeps the dotted path of a launch value', () => {
    const sections = detailSections(detail([entry(['model', 'args'], ['-m']), entry(['x'], 1)]));
    expect(sections.launch.map((row) => row.key)).toEqual(['model.args', 'x']);
  });

  it('skips a value with no path at all', () => {
    const sections = detailSections(detail([entry([], 1), entry(['x'], 1)]));
    expect(sections.launch.map((row) => row.key)).toEqual(['x']);
  });

  it('puts a bare env value among the launch values, since it names no variable', () => {
    const sections = detailSections(detail([entry(['env'], { A: '1' })]));
    expect(sections.environment).toEqual([]);
    expect(sections.launch.map((row) => row.key)).toEqual(['env']);
  });

  it('puts a bare aliases table among the launch values, since it names no alias', () => {
    const sections = detailSections(detail([entry(['model', 'aliases'], { a: 'b' })]));
    expect(sections.aliases).toEqual([]);
    expect(sections.launch.map((row) => row.key)).toEqual(['model.aliases']);
  });

  it('shows the three provided tables and nothing else of the same name', () => {
    const sections = detailSections(
      detail([
        entry(['mcp', 'a'], 1),
        entry(['hooks', 'a'], 1),
        entry(['mcp_registration', 'a'], 1),
        entry(['mcps', 'a'], 1),
      ])
    );

    expect(sections.provided.map((row) => row.key)).toEqual(['mcp', 'hooks', 'mcp_registration']);
    expect(sections.launch.map((row) => row.key)).toEqual(['mcps.a']);
  });

  it('keeps a provided line changed once a later value of the table is unchanged', () => {
    const sections = detailSections(
      detail([entry(['mcp', 'a'], 1, { changed: true }), entry(['mcp', 'b'], 2)])
    );
    expect(sections.provided).toEqual([{ key: 'mcp', text: '', shippedText: null, changed: true }]);
  });

  it('marks a provided line changed when only a later value of the table is', () => {
    const sections = detailSections(
      detail([entry(['mcp', 'a'], 1), entry(['mcp', 'b'], 2, { changed: true })])
    );
    expect(sections.provided[0]?.changed).toBe(true);
  });
});

describe('isCommandDirty and canSaveCommand', () => {
  it.each<[string, string[], string[], boolean]>([
    ['the same argv', ['a', 'b'], ['a', 'b'], false],
    ['a changed argument', ['a', 'c'], ['a', 'b'], true],
    ['an added argument', ['a', 'b', 'c'], ['a', 'b'], true],
    ['a removed argument', ['a'], ['a', 'b'], true],
  ])('reads %s as dirty=%s', (_label, edited, saved, dirty) => {
    expect(isCommandDirty(edited, saved)).toBe(dirty);
  });

  it('lets a changed argv with only non-empty arguments be saved', () => {
    expect(canSaveCommand(['a', 'c'], ['a', 'b'])).toBe(true);
  });

  it('refuses an empty argv even though it differs from the saved one', () => {
    expect(canSaveCommand([], ['a'])).toBe(false);
  });

  it('refuses an empty argument anywhere in the argv', () => {
    expect(canSaveCommand(['', 'b'], ['a', 'b'])).toBe(false);
    expect(canSaveCommand(['a', ''], ['a', 'b'])).toBe(false);
  });
});

describe('platformName', () => {
  it('names each platform the way the command heading says it', () => {
    expect([platformName('windows'), platformName('mac'), platformName('linux')]).toEqual([
      'Windows',
      'macOS',
      'Linux',
    ]);
  });
});

describe('detailSections: more shapes', () => {
  it('leaves a provided line unchanged when none of its values is', () => {
    const sections = detailSections(detail([entry(['mcp', 'a'], 1), entry(['mcp', 'b'], 2)]));
    expect(sections.provided[0]?.changed).toBe(false);
  });

  it('puts a deeper path that is not an alias table among the launch values', () => {
    const sections = detailSections(detail([entry(['model', 'args', 'x'], 1)]));
    expect(sections.aliases).toEqual([]);
    expect(sections.launch.map((row) => row.key)).toEqual(['model.args.x']);
  });
});
