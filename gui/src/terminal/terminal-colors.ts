/**
 * Resolves the colors of a frame's cells against the window's theme: the
 * theme's 16 colors, the xterm 256-color cube and grays, and whatever the
 * program in the window set over them (OSC 4/10/11/12).
 */
import { ANSI_THEME_KEYS, THEME_TOKENS, type TerminalTheme } from '../components/terminal-theme';
import type { CellAttr, CellColor, ColorOverride, Rgb } from '../ipc/session-boundary';
import type { WirePalette } from '../ipc/session-client';

/**
 * Colors a theme falls back to for a token that resolved to nothing; the
 * dark theme's values (`tokens.css`).
 */
const FALLBACK_THEME: TerminalTheme = {
  background: '#1e1e1e',
  foreground: '#ffffff',
  cursor: '#4a9eff',
  selectionBackground: '#264f78',
  selectionForeground: '#ffffff',
  selectionInactiveBackground: '#3d3d3d',
  black: '#000000',
  red: '#cd3131',
  green: '#0dbc79',
  yellow: '#e5e510',
  blue: '#007acc',
  magenta: '#bc3fbc',
  cyan: '#11a8cd',
  white: '#e5e5e5',
  brightBlack: '#666666',
  brightRed: '#f14c4c',
  brightGreen: '#23d18b',
  brightYellow: '#f5f543',
  brightBlue: '#3b8eea',
  brightMagenta: '#d670d6',
  brightCyan: '#29b8db',
  brightWhite: '#ffffff',
};

/** A theme with every color present. */
export function completeTheme(theme: Partial<TerminalTheme>): TerminalTheme {
  const complete: TerminalTheme = { ...FALLBACK_THEME };
  for (const key of Object.keys(THEME_TOKENS) as (keyof TerminalTheme)[]) {
    const value = theme[key];
    if (value !== undefined) {
      complete[key] = value;
    }
  }
  return complete;
}

const CUBE_LEVELS = [0, 95, 135, 175, 215, 255] as const;

function rgbCss([red, green, blue]: Rgb): string {
  return `rgb(${red}, ${green}, ${blue})`;
}

/** Entry `index` of the 256-color palette beyond the theme's 16. */
function extendedColor(index: number): string {
  if (index < 232) {
    const cube = index - 16;
    const level = (value: number): number => CUBE_LEVELS[value] ?? 0;
    return rgbCss([level(Math.floor(cube / 36)), level(Math.floor(cube / 6) % 6), level(cube % 6)]);
  }
  const gray = 8 + (index - 232) * 10;
  return rgbCss([gray, gray, gray]);
}

/** The colors a grid is drawn with: the theme with the program's overrides over it. */
export interface ResolvedPalette {
  theme: TerminalTheme;
  foreground: string;
  background: string;
  cursor: string;
  /** Colors 0-255. */
  indexed: string[];
}

/** Applies the program's overrides over the theme. */
export function resolvePalette(theme: TerminalTheme, overrides: ColorOverride[]): ResolvedPalette {
  const indexed = Array.from({ length: 256 }, (_, index) => {
    const key = ANSI_THEME_KEYS[index];
    return key === undefined ? extendedColor(index) : theme[key];
  });
  const resolved: ResolvedPalette = {
    theme,
    foreground: theme.foreground,
    background: theme.background,
    cursor: theme.cursor,
    indexed,
  };
  for (const { slot, rgb } of overrides) {
    const css = rgbCss(rgb);
    if (slot === 'foreground') resolved.foreground = css;
    else if (slot === 'background') resolved.background = css;
    else if (slot === 'cursor') resolved.cursor = css;
    else resolved.indexed[slot.indexed] = css;
  }
  return resolved;
}

function colorOf(color: CellColor, palette: ResolvedPalette, fallback: string): string {
  if (color === 'default') return fallback;
  if ('indexed' in color) return palette.indexed[color.indexed] ?? fallback;
  return rgbCss(color.rgb);
}

/** The colors one cell is painted with. */
export interface CellPaint {
  foreground: string;
  /** `null` when the cell shows the default background. */
  background: string | null;
  /** Drawn at half opacity (SGR 2). */
  isDim: boolean;
}

/**
 * The colors of a cell with `attr`. Bold makes colors 0-7 bright, as
 * xterm.js does by default; a selected cell takes the selection colors.
 */
export function paintOf(attr: CellAttr, palette: ResolvedPalette, isFocused: boolean): CellPaint {
  const flags = new Set(attr.flags);
  if (flags.has('selected')) {
    return {
      foreground: palette.theme.selectionForeground,
      background: isFocused
        ? palette.theme.selectionBackground
        : palette.theme.selectionInactiveBackground,
      isDim: false,
    };
  }
  let fgColor = attr.fg;
  if (flags.has('bold') && typeof fgColor === 'object' && 'indexed' in fgColor) {
    if (fgColor.indexed < 8) fgColor = { indexed: fgColor.indexed + 8 };
  }
  let foreground = colorOf(fgColor, palette, palette.foreground);
  let background: string | null =
    attr.bg === 'default' ? null : colorOf(attr.bg, palette, palette.background);
  if (flags.has('inverse')) {
    const swapped = background ?? palette.background;
    background = foreground;
    foreground = swapped;
  }
  if (flags.has('hidden')) {
    foreground = background ?? palette.background;
  }
  return { foreground, background, isDim: flags.has('dim') };
}

/** `#rrggbb` as a wire color; `null` for anything else. */
function hexToRgb(value: string): Rgb | null {
  const match = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(value.trim());
  if (match === null) return null;
  const [, red, green, blue] = match;
  return [parseInt(red ?? '0', 16), parseInt(green ?? '0', 16), parseInt(blue ?? '0', 16)];
}

/** Paints of one frame: each distinct attribute object is painted once. */
export class PaintCache {
  private readonly paints = new Map<CellAttr, CellPaint>();

  constructor(
    private readonly palette: ResolvedPalette,
    private readonly isFocused: boolean
  ) {}

  of(attr: CellAttr): CellPaint {
    let paint = this.paints.get(attr);
    if (paint === undefined) {
      paint = paintOf(attr, this.palette, this.isFocused);
      this.paints.set(attr, paint);
    }
    return paint;
  }
}

/** The theme as the daemon answers color queries with it. */
export function wirePaletteOf(theme: TerminalTheme): WirePalette {
  const fallback = (value: string, key: keyof TerminalTheme): Rgb =>
    hexToRgb(value) ?? hexToRgb(FALLBACK_THEME[key]) ?? [0, 0, 0];
  return {
    foreground: fallback(theme.foreground, 'foreground'),
    background: fallback(theme.background, 'background'),
    cursor: fallback(theme.cursor, 'cursor'),
    ansi: ANSI_THEME_KEYS.map((key) => fallback(theme[key], key)),
  };
}
