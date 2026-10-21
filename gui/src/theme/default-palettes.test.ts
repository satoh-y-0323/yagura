import { describe, it, expect } from 'vitest';

import type { Rgb } from '../ipc/session-boundary';
import { defaultPalettesOf } from './default-palettes';
import { BUILT_IN_PALETTES, PALETTE_TOKEN_KEYS } from './palette';

function paletteOf(colour: string): Record<string, string> {
  const palette: Record<string, string> = {};
  for (const key of PALETTE_TOKEN_KEYS) {
    palette[key] = colour;
  }
  return palette;
}

function rgbOf(hex: string | undefined): Rgb {
  const value = hex ?? '';
  return [1, 3, 5].map((at) => parseInt(value.slice(at, at + 2), 16)) as Rgb;
}

describe('defaultPalettesOf', () => {
  it('gives each vendor its assigned palette, leaves out the rest, and keeps the app palette', () => {
    const palettes = defaultPalettesOf({
      appPalette: BUILT_IN_PALETTES.dark,
      userPalettes: { mine: paletteOf('#123456') },
      vendorAssignments: {
        'vendor-light': 'light',
        'vendor-mine': 'mine',
        'vendor-missing': 'no-such-theme',
      },
      windowOverrides: {},
    });

    expect(palettes.app.background).toEqual(rgbOf(BUILT_IN_PALETTES.dark['color-background']));
    expect(palettes.app.foreground).toEqual(rgbOf(BUILT_IN_PALETTES.dark['color-foreground']));
    expect(Object.keys(palettes.vendors).sort()).toEqual(['vendor-light', 'vendor-mine']);
    expect(palettes.vendors['vendor-light']?.background).toEqual(
      rgbOf(BUILT_IN_PALETTES.light['color-background'])
    );
    const mine = palettes.vendors['vendor-mine'];
    expect(mine?.foreground).toEqual([0x12, 0x34, 0x56]);
    expect(mine?.background).toEqual([0x12, 0x34, 0x56]);
    expect(mine?.ansi).toEqual(Array.from({ length: 16 }, () => [0x12, 0x34, 0x56]));
  });
});
