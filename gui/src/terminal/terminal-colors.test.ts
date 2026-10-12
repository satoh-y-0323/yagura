import { describe, it, expect } from 'vitest';
import { ANSI_THEME_KEYS, type TerminalTheme } from '../components/terminal-theme';
import type { CellAttr } from '../ipc/session-boundary';
import {
  completeTheme,
  PaintCache,
  paintOf,
  resolvePalette,
  wirePaletteOf,
  type ResolvedPalette,
} from './terminal-colors';

// Every color distinct, so a lookup of the wrong one shows.
const THEME: TerminalTheme = {
  background: '#101010',
  foreground: '#f0f0f0',
  cursor: '#00ff00',
  selectionBackground: '#112233',
  selectionForeground: '#ddeeff',
  selectionInactiveBackground: '#445566',
  black: '#000001',
  red: '#000002',
  green: '#000003',
  yellow: '#000004',
  blue: '#000005',
  magenta: '#000006',
  cyan: '#000007',
  white: '#000008',
  brightBlack: '#000009',
  brightRed: '#00000a',
  brightGreen: '#00000b',
  brightYellow: '#00000c',
  brightBlue: '#00000d',
  brightMagenta: '#00000e',
  brightCyan: '#00000f',
  brightWhite: '#000010',
};

function attr(fields: Partial<CellAttr>): CellAttr {
  return { fg: 'default', bg: 'default', flags: [], ...fields };
}

describe('a theme with every color present', () => {
  it('takes the colors it is given and fills the rest from the dark theme', () => {
    const complete = completeTheme({ background: '#abcdef', red: '#123456' });
    expect(complete.background).toBe('#abcdef');
    expect(complete.red).toBe('#123456');
    expect(complete.foreground).toBe('#ffffff');
    expect(complete.brightWhite).toBe('#ffffff');
    expect(complete.selectionInactiveBackground).toBe('#3d3d3d');
    expect(Object.keys(complete)).toHaveLength(22);
  });

  it('takes every color of a complete theme', () => {
    expect(completeTheme(THEME)).toEqual(THEME);
  });

  it('ignores a color the theme does not have', () => {
    const complete = completeTheme({ background: '#abcdef', ...{ unknown: '#000000' } });
    expect(complete).not.toHaveProperty('unknown');
  });
});

describe('the palette a grid is drawn with', () => {
  it('puts the theme in the first 16 colors and the theme colors beside them', () => {
    const palette = resolvePalette(THEME, []);
    expect(palette.indexed.slice(0, 16)).toEqual(ANSI_THEME_KEYS.map((key) => THEME[key]));
    expect(palette.indexed).toHaveLength(256);
    expect(palette.foreground).toBe('#f0f0f0');
    expect(palette.background).toBe('#101010');
    expect(palette.cursor).toBe('#00ff00');
    expect(palette.theme).toBe(THEME);
  });

  it('fills 16-231 with the 6x6x6 color cube', () => {
    const { indexed } = resolvePalette(THEME, []);
    expect(indexed[16]).toBe('rgb(0, 0, 0)');
    expect(indexed[17]).toBe('rgb(0, 0, 95)');
    expect(indexed[21]).toBe('rgb(0, 0, 255)');
    expect(indexed[22]).toBe('rgb(0, 95, 0)');
    expect(indexed[52]).toBe('rgb(95, 0, 0)');
    expect(indexed[16 + 36 * 2 + 6 * 3 + 4]).toBe('rgb(135, 175, 215)');
    expect(indexed[231]).toBe('rgb(255, 255, 255)');
  });

  it('fills 232-255 with grays from 8 in steps of 10', () => {
    const { indexed } = resolvePalette(THEME, []);
    expect(indexed[232]).toBe('rgb(8, 8, 8)');
    expect(indexed[233]).toBe('rgb(18, 18, 18)');
    expect(indexed[255]).toBe('rgb(238, 238, 238)');
  });

  it('puts the colors a program set over the theme', () => {
    const palette = resolvePalette(THEME, [
      { slot: 'foreground', rgb: [1, 2, 3] },
      { slot: 'background', rgb: [4, 5, 6] },
      { slot: 'cursor', rgb: [7, 8, 9] },
      { slot: { indexed: 1 }, rgb: [10, 11, 12] },
      { slot: { indexed: 200 }, rgb: [13, 14, 15] },
    ]);
    expect(palette.foreground).toBe('rgb(1, 2, 3)');
    expect(palette.background).toBe('rgb(4, 5, 6)');
    expect(palette.cursor).toBe('rgb(7, 8, 9)');
    expect(palette.indexed[1]).toBe('rgb(10, 11, 12)');
    expect(palette.indexed[200]).toBe('rgb(13, 14, 15)');
    expect(palette.indexed[2]).toBe('#000003');
  });

  it('does not carry one window’s overrides into the next palette', () => {
    resolvePalette(THEME, [{ slot: { indexed: 1 }, rgb: [10, 11, 12] }]);
    expect(resolvePalette(THEME, []).indexed[1]).toBe('#000002');
  });
});

describe('the colors a cell is painted with', () => {
  const palette: ResolvedPalette = resolvePalette(THEME, []);

  it('paints a plain cell in the default colors with no background of its own', () => {
    expect(paintOf(attr({}), palette, true)).toEqual({
      foreground: '#f0f0f0',
      background: null,
      isDim: false,
    });
  });

  it('paints indexed and true colors', () => {
    expect(paintOf(attr({ fg: { indexed: 2 }, bg: { indexed: 4 } }), palette, true)).toEqual({
      foreground: '#000003',
      background: '#000005',
      isDim: false,
    });
    expect(
      paintOf(attr({ fg: { rgb: [1, 2, 3] }, bg: { rgb: [4, 5, 6] } }), palette, true)
    ).toEqual({ foreground: 'rgb(1, 2, 3)', background: 'rgb(4, 5, 6)', isDim: false });
  });

  it('falls back to the default color for an index beyond the palette', () => {
    const paint = paintOf(attr({ fg: { indexed: 300 }, bg: { indexed: 300 } }), palette, true);
    expect(paint.foreground).toBe('#f0f0f0');
    expect(paint.background).toBe('#101010');
  });

  it('makes bold colors 0-7 bright, as xterm.js does, and leaves the rest', () => {
    const bold = (fg: CellAttr['fg']) => paintOf(attr({ fg, flags: ['bold'] }), palette, true);
    expect(bold({ indexed: 0 }).foreground).toBe('#000009');
    expect(bold({ indexed: 7 }).foreground).toBe('#000010');
    expect(bold({ indexed: 8 }).foreground).toBe('#000009');
    expect(bold({ indexed: 100 }).foreground).toBe(palette.indexed[100]);
    expect(bold({ rgb: [1, 2, 3] }).foreground).toBe('rgb(1, 2, 3)');
    expect(bold('default').foreground).toBe('#f0f0f0');
    expect(paintOf(attr({ fg: { indexed: 1 } }), palette, true).foreground).toBe('#000002');
  });

  it('swaps the colors of an inverse cell, the default background standing for the theme’s', () => {
    expect(paintOf(attr({ flags: ['inverse'] }), palette, true)).toEqual({
      foreground: '#101010',
      background: '#f0f0f0',
      isDim: false,
    });
    expect(
      paintOf(attr({ fg: { indexed: 1 }, bg: { indexed: 2 }, flags: ['inverse'] }), palette, true)
    ).toEqual({ foreground: '#000003', background: '#000002', isDim: false });
  });

  it('hides a hidden cell’s text in its background', () => {
    expect(paintOf(attr({ flags: ['hidden'] }), palette, true).foreground).toBe('#101010');
    expect(paintOf(attr({ bg: { indexed: 2 }, flags: ['hidden'] }), palette, true).foreground).toBe(
      '#000003'
    );
  });

  it('hides an inverse hidden cell in the swapped background', () => {
    expect(paintOf(attr({ flags: ['inverse', 'hidden'] }), palette, true).foreground).toBe(
      '#f0f0f0'
    );
  });

  it('dims a dim cell', () => {
    expect(paintOf(attr({ flags: ['dim'] }), palette, true).isDim).toBe(true);
  });

  it('paints a selected cell in the selection colors, dimmer when the window is not focused', () => {
    const selected = attr({ fg: { indexed: 1 }, bg: { indexed: 2 }, flags: ['selected', 'dim'] });
    expect(paintOf(selected, palette, true)).toEqual({
      foreground: '#ddeeff',
      background: '#112233',
      isDim: false,
    });
    expect(paintOf(selected, palette, false).background).toBe('#445566');
  });
});

describe('the theme as the daemon answers color queries with it', () => {
  it('reads each color as red, green and blue, with the 16 ANSI colors in order', () => {
    const wire = wirePaletteOf(THEME);
    expect(wire.foreground).toEqual([0xf0, 0xf0, 0xf0]);
    expect(wire.background).toEqual([0x10, 0x10, 0x10]);
    expect(wire.cursor).toEqual([0x00, 0xff, 0x00]);
    expect(wire.ansi).toHaveLength(16);
    expect(wire.ansi[0]).toEqual([0, 0, 1]);
    expect(wire.ansi[15]).toEqual([0, 0, 0x10]);
  });

  it('accepts capital letters and surrounding spaces', () => {
    const wire = wirePaletteOf({ ...THEME, foreground: ' #AABBCC ' });
    expect(wire.foreground).toEqual([0xaa, 0xbb, 0xcc]);
  });

  it('answers with the dark theme’s color for text around a hex color', () => {
    const wire = wirePaletteOf({ ...THEME, foreground: 'x#aabbcc', background: '#aabbccdd' });
    expect(wire.foreground).toEqual([0xff, 0xff, 0xff]);
    expect(wire.background).toEqual([0x1e, 0x1e, 0x1e]);
  });

  it('answers with the dark theme’s color for one that is not #rrggbb', () => {
    const wire = wirePaletteOf({
      ...THEME,
      foreground: 'rgb(1, 2, 3)',
      background: '#12345',
      cursor: '',
      red: 'red',
    });
    expect(wire.foreground).toEqual([0xff, 0xff, 0xff]);
    expect(wire.background).toEqual([0x1e, 0x1e, 0x1e]);
    expect(wire.cursor).toEqual([0x4a, 0x9e, 0xff]);
    expect(wire.ansi[1]).toEqual([0xcd, 0x31, 0x31]);
  });
});

describe('the paints of one frame', () => {
  const attr: CellAttr = { fg: { indexed: 1 }, bg: 'default', flags: ['selected'] };
  const palette = resolvePalette(THEME, []);

  it('are the colors paintOf gives for the focus', () => {
    expect(new PaintCache(palette, true).of(attr)).toEqual(paintOf(attr, palette, true));
    expect(new PaintCache(palette, false).of(attr)).toEqual(paintOf(attr, palette, false));
    expect(new PaintCache(palette, true).of(attr)).not.toEqual(
      new PaintCache(palette, false).of(attr)
    );
  });

  it('paint an attribute once', () => {
    const cache = new PaintCache(palette, true);
    expect(cache.of(attr)).toBe(cache.of(attr));
  });
});
