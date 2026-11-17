import { describe, it, expect } from 'vitest';

import { BUILT_IN_PALETTES, PALETTE_TOKEN_KEYS, type Palette } from './palette';
import { applyPalette } from './theme-apply';

// The derivation is measured through `applyPalette`, the one place L1 tokens
// leave the module, so the tests do not depend on how derive-tokens is shaped.

const UNUSED_KEYS = [
  'color-primary-hover',
  'color-error-hover',
  'color-warning-action',
  'color-warning-action-hover',
  'color-on-warning',
  'color-on-warning-action',
] as const;

const SURFACES = ['--bg-canvas', '--bg-chrome', '--bg-raised'] as const;

const THEME_NAMES = ['dark', 'light'] as const;

interface Rgba {
  r: number;
  g: number;
  b: number;
  a: number;
}

function parseColor(value: string): Rgba {
  const hex = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})?$/i.exec(value.trim());
  if (hex) {
    return {
      r: parseInt(hex[1] ?? '', 16),
      g: parseInt(hex[2] ?? '', 16),
      b: parseInt(hex[3] ?? '', 16),
      a: hex[4] === undefined ? 1 : parseInt(hex[4], 16) / 255,
    };
  }
  const fn = /^rgba?\(\s*(\d+)[\s,]+(\d+)[\s,]+(\d+)(?:\s*[,/]\s*([\d.]+%?))?\s*\)$/i.exec(
    value.trim()
  );
  if (fn) {
    const alpha = fn[4];
    const a = alpha === undefined ? 1 : alpha.endsWith('%') ? parseFloat(alpha) / 100 : +alpha;
    return { r: +(fn[1] ?? 0), g: +(fn[2] ?? 0), b: +(fn[3] ?? 0), a };
  }
  throw new Error(`not a color the test can read: ${JSON.stringify(value)}`);
}

function over(fg: Rgba, bg: Rgba): Rgba {
  const mix = (f: number, b: number) => f * fg.a + b * (1 - fg.a);
  return { r: mix(fg.r, bg.r), g: mix(fg.g, bg.g), b: mix(fg.b, bg.b), a: 1 };
}

function luminance(c: Rgba): number {
  const channel = (v: number) => {
    const s = v / 255;
    return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b);
}

function contrast(a: Rgba, b: Rgba): number {
  const la = luminance(a);
  const lb = luminance(b);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

/** Everything `applyPalette` writes for a palette, by property name. */
function written(palette: Palette): Map<string, string> {
  const map = new Map<string, string>();
  applyPalette({ style: { setProperty: (name, value) => void map.set(name, value) } }, palette);
  return map;
}

function tokenOf(map: Map<string, string>, name: string): string {
  const value = map.get(name);
  if (value === undefined) {
    throw new Error(`applyPalette wrote no ${name}`);
  }
  return value;
}

/** A token as an opaque color, laid over the canvas when it is translucent. */
function solid(map: Map<string, string>, name: string): Rgba {
  return over(parseColor(tokenOf(map, name)), parseColor(tokenOf(map, '--bg-canvas')));
}

function ratioOnSurface(map: Map<string, string>, name: string, surface: string): number {
  return contrast(over(parseColor(tokenOf(map, name)), solid(map, surface)), solid(map, surface));
}

describe.each(THEME_NAMES)('the tokens derived from the built-in %s palette', (theme) => {
  const map = (): Map<string, string> => written(BUILT_IN_PALETTES[theme]);

  it.each(['--fg-default', '--fg-muted'])('keeps %s at 4.5:1 on every surface', (name) => {
    for (const surface of SURFACES) {
      expect(ratioOnSurface(map(), name, surface), `${name} on ${surface}`).toBeGreaterThanOrEqual(
        4.5
      );
    }
  });

  it.each(['--st-approval', '--st-input', '--st-running', '--line-strong', '--ring'])(
    'keeps %s at 3:1 on every surface',
    (name) => {
      for (const surface of SURFACES) {
        expect(
          ratioOnSurface(map(), name, surface),
          `${name} on ${surface}`
        ).toBeGreaterThanOrEqual(3);
      }
    }
  );

  it.each([
    ['--on-accent', '--accent'],
    ['--on-danger', '--danger'],
  ])('keeps %s readable at 4.5:1 on %s', (on, fill) => {
    const tokens = map();
    expect(ratioOnSurface(tokens, on, fill)).toBeGreaterThanOrEqual(4.5);
  });
});

describe('the light palette’s terminal colors', () => {
  // White and bright white are the colors a terminal app uses as a ground, so a
  // ratio against the light ground is not asked of them.
  const readable = PALETTE_TOKEN_KEYS.filter(
    (key) =>
      key.startsWith('term-ansi-') && key !== 'term-ansi-white' && key !== 'term-ansi-bright-white'
  );

  it('counts the 14 colors the ratio is asked of', () => {
    expect(readable).toHaveLength(14);
  });

  it.each(readable)('keeps %s at 3:1 on the terminal ground', (key) => {
    const light = BUILT_IN_PALETTES.light;
    const ratio = contrast(
      parseColor(light[key] ?? ''),
      parseColor(light['color-background'] ?? '')
    );
    expect(ratio).toBeGreaterThanOrEqual(3);
  });
});

describe('the light-or-dark decision', () => {
  it.each(THEME_NAMES)('is %s for the built-in palette of that name', (theme) => {
    expect(tokenOf(written(BUILT_IN_PALETTES[theme]), 'color-scheme')).toBe(theme);
  });

  it.each([
    ['dark', '#ffffff', 'light'],
    ['light', '#000000', 'dark'],
  ] as const)(
    'follows the background’s luminance, not the name: %s with %s is %s',
    (base, background, scheme) => {
      const palette = { ...BUILT_IN_PALETTES[base], 'color-background': background };
      expect(tokenOf(written(palette), 'color-scheme')).toBe(scheme);
    }
  );
});

describe('the keys a palette may leave out', () => {
  it.each(THEME_NAMES)('are not read when deriving from the %s palette', (theme) => {
    const l0 = new Set<string>(PALETTE_TOKEN_KEYS.map((key) => `--${key}`));
    const base: Palette = { ...BUILT_IN_PALETTES[theme] };
    for (const key of UNUSED_KEYS) {
      delete base[key];
    }
    const scrambled: Palette = { ...base };
    for (const key of UNUSED_KEYS) {
      scrambled[key] = '#ff00ff';
    }

    const derivedOf = (palette: Palette): [string, string][] =>
      [...written(palette)].filter(([name]) => !l0.has(name)).sort(([a], [b]) => (a < b ? -1 : 1));

    const withoutKeys = derivedOf(base);
    expect(withoutKeys.map(([name]) => name)).toContain('--bg-canvas');
    expect(withoutKeys).toEqual(derivedOf(scrambled));
  });
});
