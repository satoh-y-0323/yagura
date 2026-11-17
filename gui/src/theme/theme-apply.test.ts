import { describe, it, expect, vi } from 'vitest';

import {
  BUILT_IN_PALETTES,
  PALETTE_TOKEN_KEYS,
  UNUSED_PALETTE_KEYS,
  type Palette,
} from './palette';
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

// What `applyPalette` may write beside the palette's own keys: the derived
// tokens (design names) and `color-scheme`. The tints are one per status.
const DERIVED_REQUIRED = [
  '--bg-canvas',
  '--bg-chrome',
  '--bg-raised',
  '--bg-hover',
  '--bg-pressed',
  '--bg-selected',
  '--line',
  '--line-strong',
  '--fg-default',
  '--fg-muted',
  '--fg-faint',
  '--ring',
  '--accent',
  '--on-accent',
  '--danger',
  '--on-danger',
  '--st-approval',
  '--st-input',
  '--st-running',
  '--st-halted',
  '--st-unknown',
  '--st-idle',
  '--backdrop',
  '--shadow-popover',
  '--shadow-dialog',
  'color-scheme',
];
const DERIVED_ALLOWED = [
  ...DERIVED_REQUIRED,
  ...['approval', 'input', 'running', 'halted', 'unknown', 'idle'].map(
    (status) => `--st-${status}-tint`
  ),
];

/** Checks the names written are the palette's own keys plus the derived tokens, and no others. */
function expectOnlyOwnKeysAndDerived(calledNames: string[], palette: Palette): void {
  const own = Object.keys(palette).map((key) => `--${key}`);
  const names = new Set(calledNames);
  for (const name of [...own, ...DERIVED_REQUIRED]) {
    expect(names, `${name} should be written`).toContain(name);
  }
  const allowed = new Set([...own, ...DERIVED_ALLOWED]);
  expect(calledNames.filter((name) => !allowed.has(name))).toEqual([]);
}

const UNUSED_KEYS = UNUSED_PALETTE_KEYS;

function omitKeys(palette: Palette, keys: readonly string[]): Palette {
  return Object.fromEntries(Object.entries(palette).filter(([key]) => !keys.includes(key)));
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
  it.each([
    ['a 35-key', paletteOf('#123456')],
    ['a 29-key', omitKeys(paletteOf('#123456'), UNUSED_KEYS)],
  ])('writes every key of %s palette as a custom property with its value', (_label, palette) => {
    const target = makePaletteTarget();

    applyPalette(target, palette);

    for (const key of Object.keys(palette)) {
      expect(target.setPropertyMock).toHaveBeenCalledWith(`--${key}`, '#123456');
    }
  });

  it.each([
    ['light', BUILT_IN_PALETTES.light],
    ['a 29-key', omitKeys(BUILT_IN_PALETTES.light, UNUSED_KEYS)],
  ])(
    'writes the derived tokens and color-scheme beside the keys of %s, and nothing else',
    (_label, palette) => {
      const target = makePaletteTarget();
      applyPalette(target, palette);

      const calledNames = target.setPropertyMock.mock.calls.map(
        (call: [string, string]) => call[0]
      );
      expectOnlyOwnKeysAndDerived(calledNames, palette);
    }
  );
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

    expectOnlyOwnKeysAndDerived(
      target.setPropertyMock.mock.calls.map((call: [string, string]) => call[0]),
      BUILT_IN_PALETTES.dark
    );
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
