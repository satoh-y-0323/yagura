import builtInPalettes from '../../../crates/yagura-protocol/built-in-palettes.json';

export type Palette = Record<string, string>;

type PaletteKey =
  | 'color-background'
  | 'color-foreground'
  | 'color-surface'
  | 'color-border'
  | 'color-primary'
  | 'color-primary-hover'
  | 'color-success'
  | 'color-warning'
  | 'color-error'
  | 'color-error-hover'
  | 'color-warning-action'
  | 'color-warning-action-hover'
  | 'color-on-warning'
  | 'color-on-warning-action'
  | 'color-text-primary'
  | 'color-text-secondary'
  | 'term-selection-background'
  | 'term-selection-foreground'
  | 'term-selection-inactive-background'
  | 'term-ansi-black'
  | 'term-ansi-red'
  | 'term-ansi-green'
  | 'term-ansi-yellow'
  | 'term-ansi-blue'
  | 'term-ansi-magenta'
  | 'term-ansi-cyan'
  | 'term-ansi-white'
  | 'term-ansi-bright-black'
  | 'term-ansi-bright-red'
  | 'term-ansi-bright-green'
  | 'term-ansi-bright-yellow'
  | 'term-ansi-bright-blue'
  | 'term-ansi-bright-magenta'
  | 'term-ansi-bright-cyan'
  | 'term-ansi-bright-white';

/**
 * The palette keys recognized by the application, matching the set enforced
 * by {@link validatePalette} and the wire schema.
 */
export const PALETTE_TOKEN_KEYS: PaletteKey[] = [
  'color-background',
  'color-foreground',
  'color-surface',
  'color-border',
  'color-primary',
  'color-primary-hover',
  'color-success',
  'color-warning',
  'color-error',
  'color-error-hover',
  'color-warning-action',
  'color-warning-action-hover',
  'color-on-warning',
  'color-on-warning-action',
  'color-text-primary',
  'color-text-secondary',
  'term-selection-background',
  'term-selection-foreground',
  'term-selection-inactive-background',
  'term-ansi-black',
  'term-ansi-red',
  'term-ansi-green',
  'term-ansi-yellow',
  'term-ansi-blue',
  'term-ansi-magenta',
  'term-ansi-cyan',
  'term-ansi-white',
  'term-ansi-bright-black',
  'term-ansi-bright-red',
  'term-ansi-bright-green',
  'term-ansi-bright-yellow',
  'term-ansi-bright-blue',
  'term-ansi-bright-magenta',
  'term-ansi-bright-cyan',
  'term-ansi-bright-white',
];

/**
 * The built-in palettes, always available even when the themes file is
 * absent or fails to load. The daemon reads the same file for the colors it
 * answers a window's color queries with before the GUI has sent any.
 */
export const BUILT_IN_PALETTES: Record<'dark' | 'light', Palette> = builtInPalettes;

const HEX_COLOR_PATTERN = /^#[0-9a-fA-F]{6}$/;

/**
 * Checks whether a value is a non-array object: the single definition of
 * "object-shaped" for `gui/src/theme/`, which `theme-resolve.ts` delegates to
 * rather than repeating the condition.
 */
export function isRecordLike(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * Checks whether `obj` has `key` as an own property: the single definition of
 * this check for `gui/src/theme/`.
 */
export function hasOwnKey(obj: Record<string, unknown>, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

/**
 * Checks whether a value is a valid colour in `#RRGGBB` format.
 */
export function isValidColor(value: unknown): boolean {
  if (typeof value !== 'string') {
    return false;
  }
  return HEX_COLOR_PATTERN.test(value);
}

/**
 * Validates that a value is an object holding every expected palette key with
 * a valid colour. Returns `null` when invalid.
 */
export function validatePalette(value: unknown): Palette | null {
  if (!isRecordLike(value)) {
    return null;
  }

  const palette = value;
  const keys = Object.keys(palette);
  const expectedKeySet = new Set<string>(PALETTE_TOKEN_KEYS);

  if (keys.length !== expectedKeySet.size) {
    return null;
  }

  if (!keys.every((key) => expectedKeySet.has(key))) {
    return null;
  }

  for (const k of keys) {
    if (!isValidColor(palette[k])) {
      return null;
    }
  }

  return palette as Palette;
}
