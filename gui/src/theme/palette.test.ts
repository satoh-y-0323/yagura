import { describe, it, expect } from 'vitest';
import type { Palette } from './palette';
import { PALETTE_TOKEN_KEYS, BUILT_IN_PALETTES, isValidColor, validatePalette } from './palette';

// The palette vocabulary is declared here as CSS custom property names so that
// the expected key set is derived (not hand-copied) by stripping the '--' prefix.
const CSS_VARIABLE_NAMES = [
  '--color-background',
  '--color-foreground',
  '--color-surface',
  '--color-border',
  '--color-primary',
  '--color-primary-hover',
  '--color-success',
  '--color-warning',
  '--color-error',
  '--color-error-hover',
  '--color-warning-action',
  '--color-warning-action-hover',
  '--color-on-warning',
  '--color-on-warning-action',
  '--color-text-primary',
  '--color-text-secondary',
  '--term-selection-background',
  '--term-selection-foreground',
  '--term-selection-inactive-background',
  '--term-ansi-black',
  '--term-ansi-red',
  '--term-ansi-green',
  '--term-ansi-yellow',
  '--term-ansi-blue',
  '--term-ansi-magenta',
  '--term-ansi-cyan',
  '--term-ansi-white',
  '--term-ansi-bright-black',
  '--term-ansi-bright-red',
  '--term-ansi-bright-green',
  '--term-ansi-bright-yellow',
  '--term-ansi-bright-blue',
  '--term-ansi-bright-magenta',
  '--term-ansi-bright-cyan',
  '--term-ansi-bright-white',
] as const;

const EXPECTED_KEYS: string[] = CSS_VARIABLE_NAMES.map((name) => name.replace(/^--/, ''));

// The fixtures mirror the `:root` and `@media (prefers-color-scheme: dark)` blocks
// of `styles/tokens.css`; keys the dark block does not redefine fall back to `:root`.
const LIGHT_FIXTURE: Record<string, string> = {
  'color-background': '#ffffff',
  'color-foreground': '#000000',
  'color-surface': '#f5f5f5',
  'color-border': '#e0e0e0',
  'color-primary': '#0066cc',
  'color-primary-hover': '#0052a3',
  'color-success': '#28a745',
  'color-warning': '#ffc107',
  'color-error': '#dc3545',
  'color-error-hover': '#d32f2f',
  'color-warning-action': '#ff9800',
  'color-warning-action-hover': '#f57c00',
  'color-on-warning': '#333333',
  'color-on-warning-action': '#ffffff',
  'color-text-primary': '#000000',
  'color-text-secondary': '#666666',
  'term-selection-background': '#add6ff',
  'term-selection-foreground': '#000000',
  'term-selection-inactive-background': '#e5e5e5',
  'term-ansi-black': '#000000',
  'term-ansi-red': '#cd3131',
  'term-ansi-green': '#00bc00',
  'term-ansi-yellow': '#949800',
  'term-ansi-blue': '#0451a5',
  'term-ansi-magenta': '#bc05bc',
  'term-ansi-cyan': '#0598bc',
  'term-ansi-white': '#555555',
  'term-ansi-bright-black': '#666666',
  'term-ansi-bright-red': '#cd3131',
  'term-ansi-bright-green': '#14ce14',
  'term-ansi-bright-yellow': '#b5ba00',
  'term-ansi-bright-blue': '#0451a5',
  'term-ansi-bright-magenta': '#bc05bc',
  'term-ansi-bright-cyan': '#0598bc',
  'term-ansi-bright-white': '#a5a5a5',
};

const DARK_FIXTURE: Record<string, string> = {
  'color-background': '#1e1e1e',
  'color-foreground': '#ffffff',
  'color-surface': '#2d2d2d',
  'color-border': '#404040',
  'color-primary': '#4a9eff',
  'color-primary-hover': '#2e7fd4',
  'color-success': '#28a745',
  'color-warning': '#ffc107',
  'color-error': '#dc3545',
  'color-error-hover': '#d32f2f',
  'color-warning-action': '#ff9800',
  'color-warning-action-hover': '#f57c00',
  'color-on-warning': '#333333',
  'color-on-warning-action': '#ffffff',
  'color-text-primary': '#ffffff',
  'color-text-secondary': '#a0a0a0',
  'term-selection-background': '#264f78',
  'term-selection-foreground': '#ffffff',
  'term-selection-inactive-background': '#3d3d3d',
  'term-ansi-black': '#000000',
  'term-ansi-red': '#cd3131',
  'term-ansi-green': '#0dbc79',
  'term-ansi-yellow': '#e5e510',
  'term-ansi-blue': '#007acc',
  'term-ansi-magenta': '#bc3fbc',
  'term-ansi-cyan': '#11a8cd',
  'term-ansi-white': '#e5e5e5',
  'term-ansi-bright-black': '#666666',
  'term-ansi-bright-red': '#f14c4c',
  'term-ansi-bright-green': '#23d18b',
  'term-ansi-bright-yellow': '#f5f543',
  'term-ansi-bright-blue': '#3b8eea',
  'term-ansi-bright-magenta': '#d670d6',
  'term-ansi-bright-cyan': '#29b8db',
  'term-ansi-bright-white': '#ffffff',
};

const THEME_NAMES: ('dark' | 'light')[] = ['dark', 'light'];

const HEX_COLOR = /^#[0-9a-fA-F]{6}$/;

function completeCandidate(): Record<string, string> {
  return { ...LIGHT_FIXTURE };
}

describe('PALETTE_TOKEN_KEYS', () => {
  it('lists exactly the keys derived from the 35 CSS variable names', () => {
    expect([...PALETTE_TOKEN_KEYS].sort()).toEqual([...EXPECTED_KEYS].sort());
  });

  it('contains 35 entries without duplicates', () => {
    expect(PALETTE_TOKEN_KEYS).toHaveLength(35);
    expect(new Set(PALETTE_TOKEN_KEYS).size).toBe(35);
  });
});

describe('BUILT_IN_PALETTES', () => {
  it('exposes exactly the dark and light palettes', () => {
    expect(Object.keys(BUILT_IN_PALETTES).sort()).toEqual(['dark', 'light']);
  });

  it.each(THEME_NAMES)('defines exactly the 35 palette keys for %s', (theme) => {
    expect(Object.keys(BUILT_IN_PALETTES[theme]).sort()).toEqual([...EXPECTED_KEYS].sort());
  });

  it.each(THEME_NAMES)('passes validatePalette for %s', (theme) => {
    expect(validatePalette(BUILT_IN_PALETTES[theme])).not.toBeNull();
  });

  it.each(THEME_NAMES)('uses #RRGGBB for every value of %s', (theme) => {
    const offenders = EXPECTED_KEYS.filter(
      (key) => !HEX_COLOR.test(BUILT_IN_PALETTES[theme][key] ?? '')
    );
    expect(offenders).toEqual([]);
  });

  it.each(Object.entries(LIGHT_FIXTURE))(
    'sets light.%s to the tokens.css value %s',
    (key, value) => {
      expect(BUILT_IN_PALETTES.light[key]).toBe(value);
    }
  );

  it.each(Object.entries(DARK_FIXTURE))('sets dark.%s to the tokens.css value %s', (key, value) => {
    expect(BUILT_IN_PALETTES.dark[key]).toBe(value);
  });

  it('keeps the dark and light palettes distinct', () => {
    expect(BUILT_IN_PALETTES.dark).not.toEqual(BUILT_IN_PALETTES.light);
  });
});

describe('isValidColor', () => {
  it.each(['#000000', '#ffffff', '#1e1e1e', '#add6ff', '#0451a5', '#ABCDEF', '#A1b2C3'])(
    'accepts %s',
    (value) => {
      expect(isValidColor(value)).toBe(true);
    }
  );

  it.each([
    '',
    '#',
    '#fff',
    '#ffff',
    '#fffff',
    '#fffffff',
    'ffffff',
    '#gggggg',
    '#00000g',
    'red',
    'rgb(0, 0, 0)',
    'var(--color-background)',
    ' #ffffff',
    '#ffffff ',
    '#ff ffff',
    '#ffffff#ffffff',
    '#ffffff\nx',
    'x\n#ffffff',
  ])('rejects %j', (value) => {
    expect(isValidColor(value)).toBe(false);
  });
});

describe('validatePalette', () => {
  it('returns a palette for a complete 35-key record', () => {
    const result: Palette | null = validatePalette(completeCandidate());
    expect(result).not.toBeNull();
    expect(Object.keys(result ?? {}).sort()).toEqual([...EXPECTED_KEYS].sort());
  });

  it('returns the same colour values that were supplied', () => {
    const result = validatePalette(completeCandidate());
    expect(result).not.toBeNull();
    for (const key of EXPECTED_KEYS) {
      expect(result?.[key]).toBe(LIGHT_FIXTURE[key]);
    }
  });

  it.each(EXPECTED_KEYS)('returns null when the key %s is missing', (missingKey) => {
    const candidate = completeCandidate();
    delete candidate[missingKey];
    expect(validatePalette(candidate)).toBeNull();
  });

  it('returns null when an unknown key is present', () => {
    const candidate: Record<string, string> = { ...completeCandidate(), 'color-extra': '#123456' };
    expect(validatePalette(candidate)).toBeNull();
  });

  it('returns null when one key is misspelled, so the count still matches', () => {
    const candidate: Record<string, string> = completeCandidate();
    const background = candidate['color-background'] ?? '';
    delete candidate['color-background'];
    candidate['color-backgruond'] = background;
    expect(validatePalette(candidate)).toBeNull();
  });

  it('returns null when a CSS-prefixed duplicate key is present', () => {
    const candidate: Record<string, string> = {
      ...completeCandidate(),
      '--color-background': '#123456',
    };
    expect(validatePalette(candidate)).toBeNull();
  });

  it.each(['#fff', 'red', 'rgb(0, 0, 0)', '', '#gggggg', '#ffffff '])(
    'returns null when color-background is %j',
    (badValue) => {
      const candidate: Record<string, string> = {
        ...completeCandidate(),
        'color-background': badValue,
      };
      expect(validatePalette(candidate)).toBeNull();
    }
  );

  it.each([
    ['a number', 16777215],
    ['null', null],
    ['an array', ['#ffffff']],
    ['an object', { r: 255 }],
  ])('returns null when a value is %s', (_label, badValue) => {
    const candidate: Record<string, unknown> = {
      ...completeCandidate(),
      'term-ansi-black': badValue,
    };
    expect(validatePalette(candidate)).toBeNull();
  });

  it.each([
    ['null', null],
    ['undefined', undefined],
    ['a string', '#ffffff'],
    ['a number', 42],
    ['a boolean', true],
    ['an array', []],
    ['a function', () => undefined],
  ])('returns null for %s', (_label, candidate) => {
    expect(validatePalette(candidate)).toBeNull();
  });

  it('returns null for an empty object', () => {
    expect(validatePalette({})).toBeNull();
  });
});
