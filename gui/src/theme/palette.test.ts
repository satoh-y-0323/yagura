import { describe, it, expect } from 'vitest';
import shippedPalettes from '../../../crates/yagura-protocol/built-in-palettes.json';
import type { Palette } from './palette';
import {
  PALETTE_TOKEN_KEYS,
  REQUIRED_PALETTE_KEYS,
  UNUSED_PALETTE_KEYS,
  BUILT_IN_PALETTES,
  isValidColor,
  validatePalette,
} from './palette';

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

// Accepted for compatibility with older theme files but read by nothing.
const EXPECTED_UNUSED_KEYS: string[] = [
  'color-primary-hover',
  'color-error-hover',
  'color-warning-action',
  'color-warning-action-hover',
  'color-on-warning',
  'color-on-warning-action',
];

const EXPECTED_REQUIRED_KEYS: string[] = EXPECTED_KEYS.filter(
  (key) => !EXPECTED_UNUSED_KEYS.includes(key)
);

const THEME_NAMES: ('dark' | 'light')[] = ['dark', 'light'];

/** A record holding only the required keys, taken from the built-in light palette. */
function requiredOnlyCandidate(): Record<string, string> {
  const candidate: Record<string, string> = {};
  for (const key of EXPECTED_REQUIRED_KEYS) {
    candidate[key] = BUILT_IN_PALETTES.light[key] ?? '';
  }
  return candidate;
}

/** The required keys plus the 6 accepted-but-unused ones: the shape of an older theme file. */
function completeCandidate(): Record<string, string> {
  const candidate = requiredOnlyCandidate();
  for (const key of EXPECTED_UNUSED_KEYS) {
    candidate[key] = '#010203';
  }
  return candidate;
}

describe('the palette key sets', () => {
  it('PALETTE_TOKEN_KEYS lists exactly the 35 accepted keys, without duplicates', () => {
    expect([...PALETTE_TOKEN_KEYS].sort()).toEqual([...EXPECTED_KEYS].sort());
  });

  it('REQUIRED_PALETTE_KEYS lists exactly the 29 required keys, without duplicates', () => {
    expect([...REQUIRED_PALETTE_KEYS].sort()).toEqual([...EXPECTED_REQUIRED_KEYS].sort());
  });

  it('UNUSED_PALETTE_KEYS lists exactly the 6 accepted but unused keys', () => {
    expect([...UNUSED_PALETTE_KEYS].sort()).toEqual([...EXPECTED_UNUSED_KEYS].sort());
  });
});

describe('BUILT_IN_PALETTES', () => {
  it('exposes exactly the dark and light palettes', () => {
    expect(Object.keys(BUILT_IN_PALETTES).sort()).toEqual(['dark', 'light']);
  });

  // validatePalette checks every value with isValidColor, so passing it also means
  // every value is #RRGGBB.
  it.each(THEME_NAMES)(
    'defines exactly the 29 required palette keys for %s, all valid colours',
    (theme) => {
      expect(Object.keys(BUILT_IN_PALETTES[theme]).sort()).toEqual(
        [...EXPECTED_REQUIRED_KEYS].sort()
      );
      expect(validatePalette(BUILT_IN_PALETTES[theme])).not.toBeNull();
    }
  );
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
  it('returns a palette with the same 35 keys and colour values for a complete record', () => {
    const supplied = completeCandidate();
    const result: Palette | null = validatePalette(supplied);
    expect(result).not.toBeNull();
    expect(Object.keys(result ?? {}).sort()).toEqual([...EXPECTED_KEYS].sort());
    for (const key of EXPECTED_KEYS) {
      expect(result?.[key]).toBe(supplied[key]);
    }
  });

  it.each(EXPECTED_REQUIRED_KEYS)(
    'returns null when the required key %s is missing',
    (missingKey) => {
      const candidate = completeCandidate();
      delete candidate[missingKey];
      expect(validatePalette(candidate)).toBeNull();
    }
  );

  it.each(EXPECTED_UNUSED_KEYS)(
    'returns a palette when the unused key %s is missing',
    (missingKey) => {
      const candidate = completeCandidate();
      delete candidate[missingKey];
      expect(validatePalette(candidate)).not.toBeNull();
    }
  );

  it('returns a palette for a record with only the 29 required keys', () => {
    const result = validatePalette(requiredOnlyCandidate());
    expect(result).not.toBeNull();
    expect(Object.keys(result ?? {}).sort()).toEqual([...EXPECTED_REQUIRED_KEYS].sort());
  });

  it('returns null when an unused key is present with a value that is not #RRGGBB', () => {
    for (const key of EXPECTED_UNUSED_KEYS) {
      const candidate: Record<string, string> = { ...completeCandidate(), [key]: '#fff' };
      expect(validatePalette(candidate), key).toBeNull();
    }
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
});

describe('the shipped palettes file', () => {
  // The daemon reads the same file; any other value makes the two sides break differently.
  it('names a default selection both sides understand', () => {
    expect(['dark', 'light', 'system']).toContain(shippedPalettes.default_selection);
  });
});
