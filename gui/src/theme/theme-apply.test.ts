import { describe, it, expect, vi } from 'vitest';

import { BUILT_IN_PALETTES, PALETTE_TOKEN_KEYS, type Palette } from './palette';
import { THEME_TOKENS, buildTheme, type TerminalTheme } from '../components/terminal-theme';
import type { ThemeState, ThemeStore } from './theme-store';
import {
  applyPalette,
  applyTerminalTheme,
  bindPalette,
  bindTerminalTheme,
  type PaletteTarget,
  type TerminalThemeTarget,
} from './theme-apply';

function paletteOf(colour: string): Palette {
  const palette: Record<string, string> = {};
  for (const key of PALETTE_TOKEN_KEYS) {
    palette[key] = colour;
  }
  return palette;
}

function makePaletteTarget(): PaletteTarget & {
  setPropertyMock: ReturnType<typeof vi.fn<(name: string, value: string) => void>>;
} {
  const setPropertyMock = vi.fn<(name: string, value: string) => void>();
  return {
    style: { setProperty: setPropertyMock },
    setPropertyMock,
  };
}

/** A target that keeps the last theme it was given, where the assertions read it. */
interface RecordingThemeTarget extends TerminalThemeTarget {
  options: { theme?: Partial<TerminalTheme> };
}

function makeTerminalThemeTarget(): RecordingThemeTarget {
  const target: RecordingThemeTarget = {
    options: {},
    setTheme: (theme) => {
      target.options.theme = theme;
    },
  };
  return target;
}

function expectedTerminalTheme(palette: Palette): Partial<TerminalTheme> {
  return buildTheme((token) => palette[token.replace(/^--/, '')] ?? '');
}

/** Base state fixture: no user themes, no overrides, dark app palette. */
function baseState(overrides: Partial<ThemeState> = {}): ThemeState {
  return {
    selection: 'dark',
    issues: [],
    loadFailure: null,
    appPalette: BUILT_IN_PALETTES.dark,
    userPalettes: {},
    vendorAssignments: {},
    windowOverrides: {},
    ...overrides,
  };
}

/** A minimal Pick<ThemeStore, 'subscribe'> double the applier binds against. */
function makeFakeSubscribable(initial: ThemeState): Pick<ThemeStore, 'subscribe'> & {
  emit: (next: ThemeState) => void;
  listenerCount: () => number;
} {
  let state = initial;
  const listeners = new Set<(state: ThemeState) => void>();
  return {
    subscribe: (run: (state: ThemeState) => void) => {
      listeners.add(run);
      run(state);
      return () => {
        listeners.delete(run);
      };
    },
    emit: (next: ThemeState) => {
      state = next;
      for (const listener of listeners) {
        listener(state);
      }
    },
    listenerCount: () => listeners.size,
  };
}

describe('applyPalette', () => {
  it('sets a CSS custom property, prefixed with --, for every key of a 35-key palette', () => {
    const target = makePaletteTarget();
    const palette = paletteOf('#123456');

    applyPalette(target, palette);

    expect(target.setPropertyMock).toHaveBeenCalledTimes(PALETTE_TOKEN_KEYS.length);
    for (const key of PALETTE_TOKEN_KEYS) {
      expect(target.setPropertyMock).toHaveBeenCalledWith(`--${key}`, '#123456');
    }
  });

  it("adds no calls beyond the palette's own keys", () => {
    const target = makePaletteTarget();
    applyPalette(target, BUILT_IN_PALETTES.light);

    const calledNames = target.setPropertyMock.mock.calls.map((call: [string, string]) => call[0]);
    expect(calledNames.sort()).toEqual(PALETTE_TOKEN_KEYS.map((key: string) => `--${key}`).sort());
  });
});

describe('applyTerminalTheme', () => {
  it('produces the same theme buildTheme would produce for the same palette', () => {
    const target = makeTerminalThemeTarget();

    applyTerminalTheme(target, BUILT_IN_PALETTES.dark);

    expect(target.options.theme).toEqual(expectedTerminalTheme(BUILT_IN_PALETTES.dark));
  });

  it('keeps exactly the THEME_TOKENS key set when every underlying token is present', () => {
    const target = makeTerminalThemeTarget();

    applyTerminalTheme(target, BUILT_IN_PALETTES.dark);

    expect(Object.keys(target.options.theme ?? {}).sort()).toEqual(
      Object.keys(THEME_TOKENS).sort()
    );
  });

  it('reflects a second, different palette after being called again', () => {
    const target = makeTerminalThemeTarget();

    applyTerminalTheme(target, BUILT_IN_PALETTES.dark);
    const afterDark = target.options.theme;
    applyTerminalTheme(target, BUILT_IN_PALETTES.light);

    expect(target.options.theme).toEqual(expectedTerminalTheme(BUILT_IN_PALETTES.light));
    expect(target.options.theme).not.toEqual(afterDark);
  });
});

describe('bindTerminalTheme', () => {
  it('applies the resolved palette immediately when bound', () => {
    const store = makeFakeSubscribable(baseState());
    const target = makeTerminalThemeTarget();

    bindTerminalTheme(store, { windowId: 'w1', vendor: null }, target);

    expect(target.options.theme).toEqual(expectedTerminalTheme(BUILT_IN_PALETTES.dark));
  });

  it('follows a subsequent app palette change (select())', () => {
    const store = makeFakeSubscribable(baseState());
    const target = makeTerminalThemeTarget();
    bindTerminalTheme(store, { windowId: 'w1', vendor: null }, target);

    store.emit(baseState({ selection: 'light', appPalette: BUILT_IN_PALETTES.light }));

    expect(target.options.theme).toEqual(expectedTerminalTheme(BUILT_IN_PALETTES.light));
  });

  it('follows a window override for its own window only', () => {
    const custom = paletteOf('#0a0b0c');
    const store = makeFakeSubscribable(baseState());
    const targetW1 = makeTerminalThemeTarget();
    const targetW2 = makeTerminalThemeTarget();
    bindTerminalTheme(store, { windowId: 'w1', vendor: null }, targetW1);
    bindTerminalTheme(store, { windowId: 'w2', vendor: null }, targetW2);

    store.emit(
      baseState({
        userPalettes: { custom },
        windowOverrides: { w1: 'custom' },
      })
    );

    expect(targetW1.options.theme).toEqual(expectedTerminalTheme(custom));
    expect(targetW2.options.theme).toEqual(expectedTerminalTheme(BUILT_IN_PALETTES.dark));
  });

  it('stops updating after the returned unsubscribe function has run', () => {
    const store = makeFakeSubscribable(baseState());
    const target = makeTerminalThemeTarget();
    const unbind = bindTerminalTheme(store, { windowId: 'w1', vendor: null }, target);
    const beforeUnbind = target.options.theme;

    unbind();
    store.emit(baseState({ selection: 'light', appPalette: BUILT_IN_PALETTES.light }));

    expect(target.options.theme).toEqual(beforeUnbind);
  });
});

describe('bindPalette', () => {
  it('applies resolve(state) immediately when bound', () => {
    const store = makeFakeSubscribable(baseState());
    const target = makePaletteTarget();

    bindPalette(store, (state: ThemeState) => state.appPalette, target);

    expect(target.setPropertyMock).toHaveBeenCalledTimes(PALETTE_TOKEN_KEYS.length);
    expect(target.setPropertyMock).toHaveBeenCalledWith(
      '--color-background',
      BUILT_IN_PALETTES.dark['color-background']
    );
  });

  it('follows subsequent state emissions', () => {
    const store = makeFakeSubscribable(baseState());
    const target = makePaletteTarget();
    bindPalette(store, (state: ThemeState) => state.appPalette, target);
    target.setPropertyMock.mockClear();

    store.emit(baseState({ selection: 'light', appPalette: BUILT_IN_PALETTES.light }));

    expect(target.setPropertyMock).toHaveBeenCalledWith(
      '--color-background',
      BUILT_IN_PALETTES.light['color-background']
    );
  });

  it('stops applying after the returned unsubscribe function has run', () => {
    const store = makeFakeSubscribable(baseState());
    const target = makePaletteTarget();
    const unbind = bindPalette(store, (state: ThemeState) => state.appPalette, target);
    target.setPropertyMock.mockClear();

    unbind();
    store.emit(baseState({ selection: 'light', appPalette: BUILT_IN_PALETTES.light }));

    expect(target.setPropertyMock).not.toHaveBeenCalled();
    expect(store.listenerCount()).toBe(0);
  });
});

describe('applyTerminalTheme: concrete colours', () => {
  it('gives the terminal the colours the palette holds under the token names, and only those', () => {
    const target = makeTerminalThemeTarget();

    applyTerminalTheme(target, {
      'color-background': '#112233',
      'term-ansi-red': '#aa0000',
      unrelated: '#ffffff',
    });

    expect(target.options.theme).toEqual({ background: '#112233', red: '#aa0000' });
  });

  it('gives the terminal nothing for a palette that holds none of the tokens', () => {
    const target = makeTerminalThemeTarget();

    applyTerminalTheme(target, {});

    expect(target.options.theme).toEqual({});
  });
});
