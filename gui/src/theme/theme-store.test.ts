import { describe, it, expect, vi, afterEach } from 'vitest';

import { BUILT_IN_PALETTES, PALETTE_TOKEN_KEYS } from './palette';
import type { LoadThemesResult, ThemesLoadFailure } from '../ipc/session-client';
import {
  createBrowserThemeDeps,
  createThemeStore,
  type ThemeState,
  type ThemeStore,
  type ThemeStoreDeps,
} from './theme-store';

// Palette fixtures are derived from PALETTE_TOKEN_KEYS on purpose: the literal list
// of token names stays declared in palette.test.ts only.
function paletteOf(colour: string): Record<string, string> {
  const palette: Record<string, string> = {};
  for (const key of PALETTE_TOKEN_KEYS) {
    palette[key] = colour;
  }
  return palette;
}

function emptyLoadResult(overrides: Partial<LoadThemesResult> = {}): LoadThemesResult {
  return {
    themes: {},
    vendorAssignments: {},
    defaultTheme: null,
    ...overrides,
  };
}

interface FakeDeps {
  deps: ThemeStoreDeps;
  storageReadMock: ReturnType<typeof vi.fn<() => string | null>>;
  storageWriteMock: ReturnType<typeof vi.fn<(value: string) => void>>;
  setPrefersDark: (value: boolean) => void;
  firePrefersDarkChange: () => void;
}

function makeDeps(options: { stored?: string | null; prefersDark?: boolean } = {}): FakeDeps {
  const storageReadMock = vi.fn<() => string | null>(() => options.stored ?? null);
  const storageWriteMock = vi.fn<(value: string) => void>();
  let prefersDark = options.prefersDark ?? false;
  let listener: (() => void) | null = null;
  const deps: ThemeStoreDeps = {
    prefersDark: () => prefersDark,
    onPrefersDarkChange: (run: () => void) => {
      listener = run;
      return () => {
        listener = null;
      };
    },
    storageRead: storageReadMock,
    storageWrite: storageWriteMock,
  };
  return {
    deps,
    storageReadMock,
    storageWriteMock,
    setPrefersDark: (value) => {
      prefersDark = value;
    },
    firePrefersDarkChange: () => {
      listener?.();
    },
  };
}

/** Reads the state a store currently holds via a throwaway subscription. */
function currentState(store: ThemeStore): ThemeState {
  let state: ThemeState | undefined;
  const unsubscribe = store.subscribe((next: ThemeState) => {
    state = next;
  });
  unsubscribe();
  if (state === undefined) {
    throw new Error('subscribe did not deliver a state synchronously');
  }
  return state;
}

describe('createThemeStore - subscribe delivers the current state synchronously', () => {
  it('calls a newly registered listener exactly once, immediately, with the current state', () => {
    const { deps } = makeDeps({ stored: 'dark' });
    const store = createThemeStore(deps);
    const listener = vi.fn();

    store.subscribe(listener);

    expect(listener).toHaveBeenCalledTimes(1);
    expect(listener.mock.calls[0]?.[0]).toMatchObject({ selection: 'dark' });
  });

  it('returns an unsubscribe function from subscribe', () => {
    const { deps } = makeDeps();
    const store = createThemeStore(deps);
    const unsubscribe = store.subscribe(() => {});
    expect(typeof unsubscribe).toBe('function');
  });
});

describe('createThemeStore - construction parses only the 4 stored forms', () => {
  it('restores a selection stored as "dark"', () => {
    const { deps } = makeDeps({ stored: 'dark' });
    expect(currentState(createThemeStore(deps)).selection).toBe('dark');
  });

  it('restores a selection stored as "light"', () => {
    const { deps } = makeDeps({ stored: 'light' });
    expect(currentState(createThemeStore(deps)).selection).toBe('light');
  });

  it('restores a selection stored as "system"', () => {
    const { deps } = makeDeps({ stored: 'system' });
    expect(currentState(createThemeStore(deps)).selection).toBe('system');
  });

  it('restores a named selection stored as "named:<name>"', () => {
    const { deps } = makeDeps({ stored: 'named:acme' });
    expect(currentState(createThemeStore(deps)).selection).toEqual({ named: 'acme' });
  });

  it.each([[null], [''], ['bogus'], ['named:'], ['Dark'], ['solarized-dark']])(
    'falls back to the built-in dark literal when storage holds %j',
    (stored) => {
      const { deps } = makeDeps({ stored });
      expect(currentState(createThemeStore(deps)).selection).toBe('dark');
    }
  );
});

describe('createThemeStore - loadUserThemes: exclusive 4-way branch', () => {
  it('(a) restores a stored named selection whose theme exists, ignoring the default', () => {
    const { deps } = makeDeps({ stored: 'named:custom' });
    const store = createThemeStore(deps);

    store.loadUserThemes(
      emptyLoadResult({
        themes: { custom: paletteOf('#010101') },
        defaultTheme: 'light',
      })
    );

    const state = currentState(store);
    expect(state.selection).toEqual({ named: 'custom' });
    expect(state.appPalette).toEqual(paletteOf('#010101'));
  });

  it('(b) falls back to the resolved default (mode literal) when the stored named theme is absent', () => {
    const { deps } = makeDeps({ stored: 'named:missing' });
    const store = createThemeStore(deps);

    store.loadUserThemes(emptyLoadResult({ defaultTheme: 'light' }));

    const state = currentState(store);
    expect(state.selection).toBe('light');
    expect(state.appPalette).toEqual(BUILT_IN_PALETTES.light);
  });

  it('(b) falls back to the resolved default (named) when the stored named theme is absent', () => {
    const { deps } = makeDeps({ stored: 'named:missing' });
    const store = createThemeStore(deps);

    store.loadUserThemes(
      emptyLoadResult({
        themes: { alt: paletteOf('#020202') },
        defaultTheme: 'alt',
      })
    );

    const state = currentState(store);
    expect(state.selection).toEqual({ named: 'alt' });
    expect(state.appPalette).toEqual(paletteOf('#020202'));
  });

  it('(b) falls back to the built-in dark literal when the stored named theme is absent and no default resolves', () => {
    const { deps } = makeDeps({ stored: 'named:missing' });
    const store = createThemeStore(deps);

    store.loadUserThemes(emptyLoadResult({ defaultTheme: null }));

    const state = currentState(store);
    expect(state.selection).toBe('dark');
    expect(state.appPalette).toEqual(BUILT_IN_PALETTES.dark);
  });

  it('(c) counter-example: a stored mode-literal selection (light) is kept when the default is dark', () => {
    const { deps } = makeDeps({ stored: 'light' });
    const store = createThemeStore(deps);

    store.loadUserThemes(emptyLoadResult({ defaultTheme: 'dark' }));

    const state = currentState(store);
    expect(state.selection).toBe('light');
    expect(state.appPalette).toEqual(BUILT_IN_PALETTES.light);
  });

  it('(c) a stored mode-literal selection (system) is kept when the default is a named theme', () => {
    const { deps } = makeDeps({ stored: 'system', prefersDark: true });
    const store = createThemeStore(deps);

    store.loadUserThemes(
      emptyLoadResult({
        themes: { alt: paletteOf('#050505') },
        defaultTheme: 'alt',
      })
    );

    expect(currentState(store).selection).toBe('system');
  });

  it('(d) applies the resolved default (mode literal) when nothing was stored', () => {
    const { deps } = makeDeps({ stored: null });
    const store = createThemeStore(deps);

    store.loadUserThemes(emptyLoadResult({ defaultTheme: 'light' }));

    expect(currentState(store).selection).toBe('light');
  });

  it('(d) applies the resolved default (named) when nothing was stored', () => {
    const { deps } = makeDeps({ stored: null });
    const store = createThemeStore(deps);

    store.loadUserThemes(
      emptyLoadResult({
        themes: { custom: paletteOf('#060606') },
        defaultTheme: 'custom',
      })
    );

    expect(currentState(store).selection).toEqual({ named: 'custom' });
  });

  it('(d) stays at the built-in dark literal when nothing was stored and no default resolves', () => {
    const { deps } = makeDeps({ stored: null });
    const store = createThemeStore(deps);

    store.loadUserThemes(emptyLoadResult({ defaultTheme: null }));

    expect(currentState(store).selection).toBe('dark');
  });
});

describe('createThemeStore - __proto__ named selection regression', () => {
  it('(iv) falls back to the built-in dark palette when the stored selection names "__proto__" and no such theme was loaded', () => {
    const { deps } = makeDeps({ stored: 'named:__proto__' });
    const store = createThemeStore(deps);

    store.loadUserThemes(emptyLoadResult({ defaultTheme: null }));

    const state = currentState(store);
    expect(state.appPalette).toEqual(BUILT_IN_PALETTES.dark);
  });
});

describe('createThemeStore - storageWrite is called only from select()', () => {
  it('does not call storageWrite during construction', () => {
    const { deps, storageWriteMock } = makeDeps({ stored: 'dark' });
    createThemeStore(deps);
    expect(storageWriteMock).not.toHaveBeenCalled();
  });

  it('does not call storageWrite during loadUserThemes', () => {
    const { deps, storageWriteMock } = makeDeps({ stored: null });
    const store = createThemeStore(deps);
    storageWriteMock.mockClear();

    store.loadUserThemes(emptyLoadResult({ defaultTheme: 'light' }));

    expect(storageWriteMock).not.toHaveBeenCalled();
  });

  it.each([['dark'], ['light'], ['system']] as const)(
    'writes "%s" to storage when selected',
    (form) => {
      const { deps, storageWriteMock } = makeDeps();
      const store = createThemeStore(deps);

      store.select(form);

      expect(storageWriteMock).toHaveBeenCalledWith(form);
    }
  );

  it('writes "named:<name>" to storage when a named selection is selected', () => {
    const { deps, storageWriteMock } = makeDeps();
    const store = createThemeStore(deps);

    store.select({ named: 'acme' });

    expect(storageWriteMock).toHaveBeenCalledWith('named:acme');
  });
});

describe('createThemeStore - select() recalculates the palette, delivers, and persists', () => {
  it('updates selection and appPalette immediately after select()', () => {
    const { deps } = makeDeps();
    const store = createThemeStore(deps);

    store.select('light');

    const state = currentState(store);
    expect(state.selection).toBe('light');
    expect(state.appPalette).toEqual(BUILT_IN_PALETTES.light);
  });

  it('resolves system against the current prefersDark reading at select time', () => {
    const darkPreferring = makeDeps({ prefersDark: true });
    const lightPreferring = makeDeps({ prefersDark: false });
    const darkStore = createThemeStore(darkPreferring.deps);
    const lightStore = createThemeStore(lightPreferring.deps);

    darkStore.select('system');
    lightStore.select('system');

    expect(currentState(darkStore).appPalette).toEqual(BUILT_IN_PALETTES.dark);
    expect(currentState(lightStore).appPalette).toEqual(BUILT_IN_PALETTES.light);
  });

  it('notifies an already-registered subscriber when select() is called', () => {
    const { deps } = makeDeps();
    const store = createThemeStore(deps);
    const listener = vi.fn();
    store.subscribe(listener);
    listener.mockClear();

    store.select('light');

    expect(listener).toHaveBeenCalledTimes(1);
    expect(listener.mock.calls[0]?.[0]).toMatchObject({ selection: 'light' });
  });
});

describe('createThemeStore - system selection follows onPrefersDarkChange', () => {
  it('recomputes the palette when the OS preference changes while system is selected', () => {
    const { deps, setPrefersDark, firePrefersDarkChange } = makeDeps({
      stored: 'system',
      prefersDark: false,
    });
    const store = createThemeStore(deps);
    expect(currentState(store).appPalette).toEqual(BUILT_IN_PALETTES.light);

    const listener = vi.fn();
    store.subscribe(listener);
    listener.mockClear();
    setPrefersDark(true);
    firePrefersDarkChange();

    expect(listener).toHaveBeenCalledTimes(1);
    expect(currentState(store).appPalette).toEqual(BUILT_IN_PALETTES.dark);
  });

  it('does not change the palette on a preference-change notification when a mode literal is selected', () => {
    const { deps, firePrefersDarkChange } = makeDeps({ stored: 'dark' });
    const store = createThemeStore(deps);

    firePrefersDarkChange();

    expect(currentState(store).appPalette).toEqual(BUILT_IN_PALETTES.dark);
  });
});

describe('createThemeStore - loadFailure branch', () => {
  it('maps loadUserThemes failure "not_found" to loadFailure: null', () => {
    const { deps } = makeDeps();
    const store = createThemeStore(deps);

    store.loadUserThemes(emptyLoadResult({ failure: 'not_found' }));

    expect(currentState(store).loadFailure).toBeNull();
  });

  it.each([['read_failed'], ['invalid_toml'], ['too_large']] as const)(
    'maps loadUserThemes failure "%s" through to loadFailure',
    (failure: Exclude<ThemesLoadFailure, 'not_found'>) => {
      const { deps } = makeDeps();
      const store = createThemeStore(deps);

      store.loadUserThemes(emptyLoadResult({ failure }));

      expect(currentState(store).loadFailure).toBe(failure);
    }
  );

  it('clears loadFailure back to null on a subsequent successful loadUserThemes', () => {
    const { deps } = makeDeps();
    const store = createThemeStore(deps);
    store.loadUserThemes(emptyLoadResult({ failure: 'read_failed' }));
    expect(currentState(store).loadFailure).toBe('read_failed');

    store.loadUserThemes(emptyLoadResult());

    expect(currentState(store).loadFailure).toBeNull();
  });
});

describe('createThemeStore - storage exception resilience', () => {
  it('does not throw when storageRead throws during construction', () => {
    const { deps } = makeDeps();
    deps.storageRead = () => {
      throw new Error('boom');
    };

    let store: ThemeStore | undefined;
    expect(() => {
      store = createThemeStore(deps);
    }).not.toThrow();
    expect(currentState(store as ThemeStore).selection).toBe('dark');
  });

  it('does not throw when storageWrite throws during select(), and still delivers the new state', () => {
    const { deps } = makeDeps();
    deps.storageWrite = () => {
      throw new Error('boom');
    };
    const store = createThemeStore(deps);

    expect(() => store.select('light')).not.toThrow();
    expect(currentState(store).selection).toBe('light');
  });
});

describe('createThemeStore - setWindowOverride', () => {
  it('sets a window override and notifies subscribers', () => {
    const { deps } = makeDeps();
    const store = createThemeStore(deps);
    const listener = vi.fn();
    store.subscribe(listener);
    listener.mockClear();

    store.setWindowOverride('w1', 'acme');

    expect(listener).toHaveBeenCalledTimes(1);
    expect(currentState(store).windowOverrides).toEqual({ w1: 'acme' });
  });

  it('clears a window override when passed null, and notifies subscribers', () => {
    const { deps } = makeDeps();
    const store = createThemeStore(deps);
    store.setWindowOverride('w1', 'acme');
    const listener = vi.fn();
    store.subscribe(listener);
    listener.mockClear();

    store.setWindowOverride('w1', null);

    expect(listener).toHaveBeenCalledTimes(1);
    expect('w1' in currentState(store).windowOverrides).toBe(false);
  });

  it('keeps each window override independent of the others', () => {
    const { deps } = makeDeps();
    const store = createThemeStore(deps);

    store.setWindowOverride('w1', 'acme');
    store.setWindowOverride('w2', 'zen');
    store.setWindowOverride('w1', null);

    const overrides = currentState(store).windowOverrides;
    expect('w1' in overrides).toBe(false);
    expect(overrides.w2).toBe('zen');
  });
});

describe('createThemeStore - unsubscribe stops delivery', () => {
  it('stops calling a listener once its unsubscribe function has run', () => {
    const { deps } = makeDeps();
    const store = createThemeStore(deps);
    const listener = vi.fn();
    const unsubscribe = store.subscribe(listener);
    const callsAtSubscribe = listener.mock.calls.length;

    unsubscribe();
    store.select('light');

    expect(listener.mock.calls.length).toBe(callsAtSubscribe);
  });
});

describe('createBrowserThemeDeps', () => {
  interface FakeMediaQueryList {
    matches: boolean;
    addEventListener: (type: string, listener: EventListenerOrEventListenerObject) => void;
    removeEventListener: (type: string, listener: EventListenerOrEventListenerObject) => void;
  }

  function makeFakeMatchMedia(initialMatches: boolean): {
    matchMedia: (query: string) => FakeMediaQueryList;
    queries: string[];
    setMatches: (value: boolean) => void;
    fireChange: () => void;
    listenerCount: () => number;
  } {
    let matches = initialMatches;
    const queries: string[] = [];
    const listeners = new Set<EventListenerOrEventListenerObject>();
    const mediaQueryList: FakeMediaQueryList = {
      get matches() {
        return matches;
      },
      addEventListener: (type, listener) => {
        if (type === 'change') {
          listeners.add(listener);
        }
      },
      removeEventListener: (type, listener) => {
        if (type === 'change') {
          listeners.delete(listener);
        }
      },
    };
    return {
      matchMedia: (query) => {
        queries.push(query);
        return mediaQueryList;
      },
      queries,
      setMatches: (value) => {
        matches = value;
      },
      fireChange: () => {
        for (const listener of listeners) {
          if (typeof listener === 'function') {
            listener(new Event('change'));
          } else {
            listener.handleEvent(new Event('change'));
          }
        }
      },
      listenerCount: () => listeners.size,
    };
  }

  function makeFakeLocalStorage(): {
    localStorage: Pick<Storage, 'getItem' | 'setItem'>;
    store: Map<string, string>;
  } {
    const store = new Map<string, string>();
    return {
      localStorage: {
        getItem: (key) => store.get(key) ?? null,
        setItem: (key, value) => {
          store.set(key, value);
        },
      },
      store,
    };
  }

  it("reads prefersDark from matchMedia('(prefers-color-scheme: dark)').matches", () => {
    const fake = makeFakeMatchMedia(true);
    const { localStorage } = makeFakeLocalStorage();
    const deps = createBrowserThemeDeps({ matchMedia: fake.matchMedia, localStorage });

    expect(deps.prefersDark()).toBe(true);
    expect(fake.queries).toContain('(prefers-color-scheme: dark)');
  });

  it('reads and writes the storage key "yagura.theme-selection"', () => {
    const fake = makeFakeMatchMedia(false);
    const { localStorage, store } = makeFakeLocalStorage();
    const deps = createBrowserThemeDeps({ matchMedia: fake.matchMedia, localStorage });

    deps.storageWrite('dark');

    expect(store.get('yagura.theme-selection')).toBe('dark');
    expect(deps.storageRead()).toBe('dark');
  });

  it('registers a change listener, delivers a change, and can unsubscribe it', () => {
    const fake = makeFakeMatchMedia(false);
    const { localStorage } = makeFakeLocalStorage();
    const deps = createBrowserThemeDeps({ matchMedia: fake.matchMedia, localStorage });

    const listener = vi.fn();
    const unsubscribe = deps.onPrefersDarkChange(listener);
    expect(fake.listenerCount()).toBe(1);

    fake.fireChange();
    expect(listener).toHaveBeenCalledTimes(1);

    unsubscribe();
    expect(fake.listenerCount()).toBe(0);
  });

  it('does not throw when matchMedia throws', () => {
    const { localStorage } = makeFakeLocalStorage();
    const deps = createBrowserThemeDeps({
      matchMedia: () => {
        throw new Error('boom');
      },
      localStorage,
    });

    expect(() => deps.prefersDark()).not.toThrow();
  });

  it('returns null from storageRead instead of throwing when localStorage.getItem throws', () => {
    const fake = makeFakeMatchMedia(false);
    const deps = createBrowserThemeDeps({
      matchMedia: fake.matchMedia,
      localStorage: {
        getItem: () => {
          throw new Error('boom');
        },
        setItem: () => {},
      },
    });

    expect(() => deps.storageRead()).not.toThrow();
    expect(deps.storageRead()).toBeNull();
  });

  it('does not throw when localStorage.setItem throws', () => {
    const fake = makeFakeMatchMedia(false);
    const deps = createBrowserThemeDeps({
      matchMedia: fake.matchMedia,
      localStorage: {
        getItem: () => null,
        setItem: () => {
          throw new Error('boom');
        },
      },
    });

    expect(() => deps.storageWrite('dark')).not.toThrow();
  });
});

describe('createThemeStore - state a subscriber sees', () => {
  it('starts with no issues and no load failure', () => {
    const { deps } = makeDeps();
    const state = currentState(createThemeStore(deps));

    expect(state.issues).toEqual([]);
    expect(state.loadFailure).toBeNull();
    expect(state.selection).toBe('dark');
  });

  it('keeps a stored named built-in selection when the themes file adds no theme', () => {
    const { deps } = makeDeps({ stored: 'named:light' });
    const store = createThemeStore(deps);

    store.loadUserThemes(emptyLoadResult({ defaultTheme: 'system' }));

    expect(currentState(store).selection).toEqual({ named: 'light' });
  });

  it('keeps the stored named dark selection when the themes file adds no theme', () => {
    const { deps } = makeDeps({ stored: 'named:dark' });
    const store = createThemeStore(deps);

    store.loadUserThemes(emptyLoadResult({ defaultTheme: 'light' }));

    expect(currentState(store).selection).toEqual({ named: 'dark' });
  });

  it('treats a selection made by the user as a stored one, so the file default does not replace it', () => {
    const { deps } = makeDeps();
    const store = createThemeStore(deps);

    store.select('light');
    store.loadUserThemes(emptyLoadResult({ defaultTheme: 'system' }));

    expect(currentState(store).selection).toBe('light');
  });

  it('ignores a stored value that is neither a built-in name nor a named form', () => {
    const { deps } = makeDeps({ stored: 'solarized' });
    const store = createThemeStore(deps);

    store.loadUserThemes(emptyLoadResult({ defaultTheme: 'light' }));

    expect(currentState(store).selection).toBe('light');
  });
});

describe('createThemeStore - who is told about an OS preference change', () => {
  it('tells subscribers when the selection is system', () => {
    const { deps, firePrefersDarkChange } = makeDeps({ stored: 'system' });
    const store = createThemeStore(deps);
    const listener = vi.fn();
    store.subscribe(listener);
    listener.mockClear();

    firePrefersDarkChange();

    expect(listener).toHaveBeenCalledTimes(1);
  });

  it.each(['dark', 'light', 'named:ocean'])('tells nobody when the selection is %s', (stored) => {
    const { deps, firePrefersDarkChange } = makeDeps({ stored });
    const store = createThemeStore(deps);
    const listener = vi.fn();
    store.subscribe(listener);
    listener.mockClear();

    firePrefersDarkChange();

    expect(listener).not.toHaveBeenCalled();
  });
});

describe('createBrowserThemeDeps - from the browser globals', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('reads the OS preference from the global matchMedia when none is given', () => {
    vi.stubGlobal('matchMedia', () => ({
      matches: true,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
    }));

    expect(createBrowserThemeDeps().prefersDark()).toBe(true);
  });

  it('reads and writes the global localStorage when none is given', () => {
    const items = new Map<string, string>();
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => items.get(key) ?? null,
      setItem: (key: string, value: string) => {
        items.set(key, value);
      },
    });
    const deps = createBrowserThemeDeps();

    deps.storageWrite('light');

    expect(items.get('yagura.theme-selection')).toBe('light');
    expect(deps.storageRead()).toBe('light');
  });

  it('answers light and registers nothing when matchMedia throws', () => {
    const deps = createBrowserThemeDeps({
      matchMedia: () => {
        throw new Error('no media');
      },
    });
    const listener = vi.fn();

    const remove = deps.onPrefersDarkChange(listener);

    expect(deps.prefersDark()).toBe(false);
    expect(() => remove()).not.toThrow();
  });
});
