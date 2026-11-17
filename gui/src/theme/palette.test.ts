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

const THEME_NAMES: ('dark' | 'light')[] = ['dark', 'light'];

const HEX_COLOR = /^#[0-9a-fA-F]{6}$/;

function completeCandidate(): Record<string, string> {
  return { ...BUILT_IN_PALETTES.light };
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
      expect(result?.[key]).toBe(BUILT_IN_PALETTES.light[key]);
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
