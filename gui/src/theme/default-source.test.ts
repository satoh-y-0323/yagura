import { afterEach, describe, expect, it, vi } from 'vitest';
import { themeOfPalette } from '../components/terminal-theme';
import { BUILT_IN_PALETTES } from './palette';
import type { ThemeState, ThemeStoreDeps } from './theme-store';

// The built-in default theme has one source: `default_selection` of the shared
// palettes file. These tests replace that file and re-import the modules that
// derive from it, because the file's own value (dark) would hide a place that
// hard-codes dark.
const PALETTES_JSON = '../../../crates/yagura-protocol/built-in-palettes.json';

type Selection = 'light' | 'system';

const DARK = BUILT_IN_PALETTES.dark;
const LIGHT = BUILT_IN_PALETTES.light;

// The palette a default selection names, with `system` following the OS preference.
function expectedPalette(selection: Selection, prefersDark: boolean): Record<string, string> {
  if (selection === 'light') {
    return LIGHT;
  }
  return prefersDark ? DARK : LIGHT;
}

// The terminal fallback has no OS preference to read, so `system` means dark there.
function expectedTerminalPalette(selection: Selection): Record<string, string> {
  return selection === 'light' ? LIGHT : DARK;
}

async function loadModulesWithDefault(selection: string) {
  vi.resetModules();
  vi.doMock(PALETTES_JSON, () => ({
    default: { default_selection: selection, palettes: { dark: DARK, light: LIGHT } },
  }));
  const palette = await import('./palette');
  const store = await import('./theme-store');
  const resolve = await import('./theme-resolve');
  const colors = await import('../terminal/terminal-colors');
  return { palette, store, resolve, colors };
}

function depsStoring(stored: string | null, prefersDark = false): ThemeStoreDeps {
  return {
    prefersDark: () => prefersDark,
    onPrefersDarkChange: () => () => {},
    storageRead: () => stored,
    storageWrite: () => {},
  };
}

function selectionOf(store: {
  subscribe: (run: (state: ThemeState) => void) => () => void;
}): ThemeState['selection'] {
  let selection: ThemeState['selection'] | undefined;
  store.subscribe((state) => {
    selection = state.selection;
  })();
  if (selection === undefined) {
    throw new Error('subscribe did not deliver a state synchronously');
  }
  return selection;
}

function rgbOf(hex: string | undefined): [number, number, number] {
  const match = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex ?? '');
  if (match === null) {
    throw new Error(`not #rrggbb: ${String(hex)}`);
  }
  return [parseInt(match[1] ?? '', 16), parseInt(match[2] ?? '', 16), parseInt(match[3] ?? '', 16)];
}

afterEach(() => {
  vi.doUnmock(PALETTES_JSON);
  vi.resetModules();
});

describe.each<Selection>(['light', 'system'])(
  'with default_selection %s in the palettes file',
  (selection) => {
    it('exposes the file’s default selection and the palette it names', async () => {
      const { palette } = await loadModulesWithDefault(selection);

      expect(palette.BUILT_IN_DEFAULT_SELECTION).toBe(selection);
      expect(palette.builtInDefaultPalette(true)).toEqual(expectedPalette(selection, true));
      expect(palette.builtInDefaultPalette(false)).toEqual(expectedPalette(selection, false));
    });

    it('starts the theme store on the default when nothing usable is stored', async () => {
      const { store } = await loadModulesWithDefault(selection);

      for (const stored of [null, 'bogus']) {
        expect(selectionOf(store.createThemeStore(depsStoring(stored)))).toBe(selection);
      }
    });

    it('settles on the default when the themes file names none and the stored theme is gone', async () => {
      const { store } = await loadModulesWithDefault(selection);
      const noThemes = { themes: {}, vendorAssignments: {}, defaultTheme: null };

      for (const stored of [null, 'named:missing']) {
        const created = store.createThemeStore(depsStoring(stored));
        created.loadUserThemes(noThemes);
        expect(selectionOf(created)).toBe(selection);
      }
    });

    it.each([true, false])(
      'resolves an unknown named theme to the default palette when prefersDark is %s',
      async (prefersDark) => {
        const { resolve } = await loadModulesWithDefault(selection);

        expect(resolve.resolveAppPalette({ named: 'nope' }, prefersDark, {})).toEqual(
          expectedPalette(selection, prefersDark)
        );
      }
    );

    it('fills a terminal theme’s missing colors from the default palette', async () => {
      const { colors } = await loadModulesWithDefault(selection);
      const expected = themeOfPalette(expectedTerminalPalette(selection));
      const other = themeOfPalette(selection === 'light' ? DARK : LIGHT);

      expect(colors.completeTheme({})).toMatchObject(expected);
      expect(expected.background).not.toBe(other.background);
    });

    it('answers an unreadable terminal color with the default palette’s', async () => {
      const { colors } = await loadModulesWithDefault(selection);
      const expected = themeOfPalette(expectedTerminalPalette(selection));
      const unreadable = { ...colors.completeTheme({}), foreground: 'bogus', background: '#12345' };

      const wire = colors.wirePaletteOf(unreadable);

      expect(wire.foreground).toEqual(rgbOf(expected.foreground));
      expect(wire.background).toEqual(rgbOf(expected.background));
    });
  }
);

// A misspelled value falls to dark in every place the default is read, the way
// the daemon's `default_palette_of` does, instead of leaving the GUI without a palette.
describe.each(['drak', 'Light'])('with a misspelled default_selection %s', (selection) => {
  it('exposes dark as the default selection and its palette', async () => {
    const { palette } = await loadModulesWithDefault(selection);

    expect(palette.BUILT_IN_DEFAULT_SELECTION).toBe('dark');
    expect(palette.builtInDefaultPalette(true)).toEqual(DARK);
    expect(palette.builtInDefaultPalette(false)).toEqual(DARK);
  });

  it('starts the theme store on dark', async () => {
    const { store } = await loadModulesWithDefault(selection);

    expect(selectionOf(store.createThemeStore(depsStoring(null)))).toBe('dark');
  });

  it.each([true, false])(
    'resolves an unknown named theme to the dark palette when prefersDark is %s',
    async (prefersDark) => {
      const { resolve } = await loadModulesWithDefault(selection);

      expect(resolve.resolveAppPalette({ named: 'nope' }, prefersDark, {})).toEqual(DARK);
    }
  );

  it('fills a terminal theme’s missing colors from the dark palette', async () => {
    const { colors } = await loadModulesWithDefault(selection);

    expect(colors.completeTheme({})).toMatchObject(themeOfPalette(DARK));
  });
});
