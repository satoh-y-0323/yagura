import type { Palette } from './palette';

/** The semantic (L1) tokens derived from a palette, and whether it is light or dark. */
export interface DerivedTokens {
  /** CSS custom property name (with the `--` prefix) to value. */
  tokens: Record<string, string>;
  scheme: 'light' | 'dark';
}

type Rgb = readonly [number, number, number];

const BLACK: Rgb = [0, 0, 0];
const NEAR_BLACK: Rgb = [0x11, 0x12, 0x14];
const WHITE: Rgb = [255, 255, 255];

// Relative luminance at which black and white text have the same contrast.
const LIGHT_BACKGROUND_LUMINANCE = 0.179;

// Margin over the 3:1 non-text contrast the `--line-strong` border must keep.
const LINE_STRONG_TARGET_RATIO = 3.2;

function parseHex(value: string | undefined): Rgb {
  const match = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(value ?? '');
  if (!match) {
    // Palettes are validated as #RRGGBB before they reach the applier.
    return BLACK;
  }
  return [
    parseInt(match[1] ?? '0', 16),
    parseInt(match[2] ?? '0', 16),
    parseInt(match[3] ?? '0', 16),
  ];
}

function hex2(v: number): string {
  return Math.round(Math.min(255, Math.max(0, v)))
    .toString(16)
    .padStart(2, '0');
}

function toHex(c: Rgb): string {
  return `#${hex2(c[0])}${hex2(c[1])}${hex2(c[2])}`;
}

function withAlpha(c: Rgb, alpha: number): string {
  return `${toHex(c)}${hex2(alpha * 255)}`;
}

/** `amount` of the way from `from` to `to`. */
function mix(from: Rgb, to: Rgb, amount: number): Rgb {
  return [
    from[0] + (to[0] - from[0]) * amount,
    from[1] + (to[1] - from[1]) * amount,
    from[2] + (to[2] - from[2]) * amount,
  ];
}

function luminance(c: Rgb): number {
  const channel = (v: number): number => {
    const s = v / 255;
    return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * channel(c[0]) + 0.7152 * channel(c[1]) + 0.0722 * channel(c[2]);
}

function contrast(a: Rgb, b: Rgb): number {
  const la = luminance(a);
  const lb = luminance(b);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

function readableOn(fill: Rgb): Rgb {
  return contrast(NEAR_BLACK, fill) >= contrast(WHITE, fill) ? NEAR_BLACK : WHITE;
}

/**
 * The weakest mix of background toward foreground that keeps the target
 * contrast against every surface, so a border stays visible on each of them.
 */
function strongLine(background: Rgb, foreground: Rgb, surfaces: readonly Rgb[]): Rgb {
  for (let step = 30; step <= 100; step += 1) {
    const candidate = mix(background, foreground, step / 100);
    if (surfaces.every((surface) => contrast(candidate, surface) >= LINE_STRONG_TARGET_RATIO)) {
      return candidate;
    }
  }
  return foreground;
}

/**
 * Derives the semantic tokens from the required keys of a palette. Keys a
 * palette may leave out are never read here.
 */
export function deriveTokens(palette: Palette): DerivedTokens {
  const background = parseHex(palette['color-background']);
  const foreground = parseHex(palette['color-foreground']);
  const surface = parseHex(palette['color-surface']);
  const primary = parseHex(palette['color-primary']);
  const success = parseHex(palette['color-success']);
  const warning = parseHex(palette['color-warning']);
  const error = parseHex(palette['color-error']);
  const textSecondary = parseHex(palette['color-text-secondary']);

  const light = luminance(background) > LIGHT_BACKGROUND_LUMINANCE;

  const chrome = light ? mix(background, foreground, 0.045) : mix(background, BLACK, 0.25);
  const muted = textSecondary;
  const faint = mix(muted, background, 0.35);
  const [hover, pressed, selected] = light ? [0.05, 0.09, 0.08] : [0.07, 0.11, 0.1];
  const tintAlpha = light ? 0.12 : 0.16;

  const tokens: Record<string, string> = {
    '--bg-canvas': toHex(background),
    '--bg-chrome': toHex(chrome),
    '--bg-raised': toHex(surface),
    '--bg-hover': withAlpha(foreground, hover),
    '--bg-pressed': withAlpha(foreground, pressed),
    '--bg-selected': withAlpha(foreground, selected),
    '--line': toHex(parseHex(palette['color-border'])),
    '--line-strong': toHex(strongLine(background, foreground, [background, chrome, surface])),
    '--fg-default': toHex(parseHex(palette['color-text-primary'])),
    '--fg-muted': toHex(muted),
    '--fg-faint': toHex(faint),
    '--ring': toHex(mix(foreground, background, 0.2)),
    '--accent': toHex(primary),
    '--on-accent': toHex(readableOn(primary)),
    '--danger': toHex(error),
    '--on-danger': toHex(readableOn(error)),
    '--backdrop': light ? 'rgb(20 20 24 / 0.32)' : 'rgb(0 0 0 / 0.55)',
    '--shadow-popover': `0 8px 24px rgb(0 0 0 / ${light ? '0.12' : '0.40'})`,
    '--shadow-dialog': `0 20px 48px rgb(0 0 0 / ${light ? '0.18' : '0.50'})`,
  };

  const statuses: [string, Rgb][] = [
    ['approval', warning],
    ['input', primary],
    ['running', success],
    ['halted', muted],
    ['unknown', muted],
    ['idle', faint],
  ];
  for (const [name, color] of statuses) {
    tokens[`--st-${name}`] = toHex(color);
    tokens[`--st-${name}-tint`] = withAlpha(color, tintAlpha);
  }

  return { tokens, scheme: light ? 'light' : 'dark' };
}
