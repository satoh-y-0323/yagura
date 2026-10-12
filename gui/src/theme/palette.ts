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
 * absent or fails to load.
 */
export const BUILT_IN_PALETTES: Record<'dark' | 'light', Palette> = {
  light: {
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
  },
  dark: {
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
  },
};

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
