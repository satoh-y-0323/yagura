import { describe, it, expect } from 'vitest';
import { THEME_TOKENS, buildTheme, type TerminalTheme } from './terminal-theme';

const ANSI_KEYS = [
  'black',
  'red',
  'green',
  'yellow',
  'blue',
  'magenta',
  'cyan',
  'white',
  'brightBlack',
  'brightRed',
  'brightGreen',
  'brightYellow',
  'brightBlue',
  'brightMagenta',
  'brightCyan',
  'brightWhite',
] as const;

const BASE_KEYS = ['background', 'foreground', 'cursor'] as const;
const SELECTION_KEYS = [
  'selectionBackground',
  'selectionForeground',
  'selectionInactiveBackground',
] as const;

const EXPECTED_KEYS = [...BASE_KEYS, ...SELECTION_KEYS, ...ANSI_KEYS];

function toKebabCase(key: string): string {
  return key.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`);
}

describe('THEME_TOKENS', () => {
  it('maps background to --color-background', () => {
    expect(THEME_TOKENS.background).toBe('--color-background');
  });

  it('maps foreground to --color-foreground', () => {
    expect(THEME_TOKENS.foreground).toBe('--color-foreground');
  });

  it('maps cursor to --color-primary', () => {
    expect(THEME_TOKENS.cursor).toBe('--color-primary');
  });

  it('maps selectionBackground to --term-selection-background', () => {
    expect(THEME_TOKENS.selectionBackground).toBe('--term-selection-background');
  });

  it('maps selectionForeground to --term-selection-foreground', () => {
    expect(THEME_TOKENS.selectionForeground).toBe('--term-selection-foreground');
  });

  it('maps selectionInactiveBackground to --term-selection-inactive-background', () => {
    expect(THEME_TOKENS.selectionInactiveBackground).toBe('--term-selection-inactive-background');
  });

  it.each(ANSI_KEYS)('maps ANSI key %s to --term-ansi-<kebab-case>', (key) => {
    expect(THEME_TOKENS[key]).toBe(`--term-ansi-${toKebabCase(key)}`);
  });

  it('defines exactly the documented keys and no others', () => {
    expect(Object.keys(THEME_TOKENS).sort()).toEqual([...EXPECTED_KEYS].sort());
  });
});

describe('buildTheme', () => {
  it('writes readToken results through for every THEME_TOKENS entry', () => {
    const theme = buildTheme((token) => `value-for-${token}`);
    for (const key of Object.keys(THEME_TOKENS)) {
      expect(theme[key as keyof TerminalTheme]).toBe(`value-for-${THEME_TOKENS[key]}`);
    }
  });

  it('passes each THEME_TOKENS token name to readToken', () => {
    const seenTokens: string[] = [];
    buildTheme((token) => {
      seenTokens.push(token);
      return 'x';
    });
    expect(seenTokens.sort()).toEqual(Object.values(THEME_TOKENS).sort());
  });

  it('omits a key from the built theme when its token resolves to an empty string', () => {
    const theme = buildTheme((token) => (token === '--color-background' ? '' : 'x'));
    expect('background' in theme).toBe(false);
    expect(theme.foreground).toBe('x');
  });

  it('returns an empty theme when every token resolves to an empty string', () => {
    const theme = buildTheme(() => '');
    expect(Object.keys(theme)).toEqual([]);
  });

  it('does not add keys beyond THEME_TOKENS', () => {
    const theme = buildTheme(() => '#000000');
    expect(Object.keys(theme).sort()).toEqual(Object.keys(THEME_TOKENS).sort());
  });
});
