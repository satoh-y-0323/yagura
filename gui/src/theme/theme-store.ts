import type { LoadThemesResult, ThemesLoadFailure } from '../ipc/session-client';
import { createGuardedLocalStorage } from '../storage/guarded-storage';
import { hasOwnKey, type Palette } from './palette';
import {
  parseUserThemes,
  isBuiltInSelection,
  resolveAppPalette,
  type AppThemeSelection,
  type ThemeIssue,
  type WindowPaletteSource,
} from './theme-resolve';

/** Key the application theme selection is persisted under. */
const STORAGE_KEY = 'yagura.theme-selection';

/** Media query the `system` selection follows. */
const PREFERS_DARK_QUERY = '(prefers-color-scheme: dark)';

/** Prefix of the stored form of a `{ named }` selection. */
const NAMED_PREFIX = 'named:';

/** The environment the store reads the OS preference and the selection from. */
export interface ThemeStoreDeps {
  /** Whether the OS asks for a dark colour scheme. */
  prefersDark(): boolean;
  /** Registers a listener for OS preference changes; returns its remover. */
  onPrefersDarkChange(listener: () => void): () => void;
  /** The persisted selection, `null` when nothing is stored. */
  storageRead(): string | null;
  /** Persists a selection. */
  storageWrite(value: string): void;
}

/** Everything the view and the appliers need to paint one application state. */
export interface ThemeState extends WindowPaletteSource {
  /** What the user selected, in the four-form selection vocabulary. */
  selection: AppThemeSelection;
  /** What was rejected while parsing the themes file. */
  issues: ThemeIssue[];
  /**
   * Why the themes file could not be read, `null` when it was read or when it
   * is simply absent (`not_found` is the normal way of running without one).
   */
  loadFailure: Exclude<ThemesLoadFailure, 'not_found'> | null;
}

/** Store of {@link ThemeState} with the operations the theme UI needs. */
export interface ThemeStore {
  /** Subscribes to the state; the listener is called once, synchronously. */
  subscribe(run: (state: ThemeState) => void): () => void;
  /** Applies and persists an application theme selection. */
  select(selection: AppThemeSelection): void;
  /** Overrides one window's palette, or clears it when `themeName` is `null`. */
  setWindowOverride(windowId: string, themeName: string | null): void;
  /** Takes in what the themes file contributed. */
  loadUserThemes(result: LoadThemesResult): void;
}

/** Parses a stored selection; `null` when it is not one of the known forms. */
function parseStoredSelection(stored: string | null): AppThemeSelection | null {
  if (stored === null) {
    return null;
  }
  if (isBuiltInSelection(stored)) {
    return stored;
  }
  if (stored.startsWith(NAMED_PREFIX)) {
    const named = stored.slice(NAMED_PREFIX.length);
    return named === '' ? null : { named };
  }
  return null;
}

/** The stored form of a selection; the inverse of {@link parseStoredSelection}. */
function serializeSelection(selection: AppThemeSelection): string {
  return typeof selection === 'string' ? selection : `${NAMED_PREFIX}${selection.named}`;
}

function readStoredSelection(deps: ThemeStoreDeps): string | null {
  try {
    return deps.storageRead();
  } catch {
    // An unreadable storage cannot be told apart from an empty one, and the
    // built-in default covers both.
    return null;
  }
}

/**
 * Creates the application theme store.
 *
 * Initialization has two stages: construction only parses the
 * stored selection, because whether a named theme exists is not known until
 * the themes file has been read; {@link ThemeStore.loadUserThemes} then
 * settles the selection against what the file contributed.
 */
export function createThemeStore(deps: ThemeStoreDeps): ThemeStore {
  const listeners = new Set<(state: ThemeState) => void>();

  const storedSelection = parseStoredSelection(readStoredSelection(deps));
  let hasStoredSelection = storedSelection !== null;

  let selection: AppThemeSelection = storedSelection ?? 'dark';
  // Null-prototype, matching `parseUserThemes`'s output shape, so the window
  // between construction and `loadUserThemes` carries the same guarantee.
  let userPalettes: Record<string, Palette> = Object.create(null) as Record<string, Palette>;
  let vendorAssignments: Record<string, string> = Object.create(null) as Record<string, string>;
  let windowOverrides: Record<string, string | null> = Object.create(null) as Record<
    string,
    string | null
  >;
  let issues: ThemeIssue[] = [];
  let loadFailure: Exclude<ThemesLoadFailure, 'not_found'> | null = null;

  const buildState = (): ThemeState => ({
    selection,
    issues,
    loadFailure,
    appPalette: resolveAppPalette(selection, deps.prefersDark(), userPalettes),
    userPalettes,
    vendorAssignments,
    windowOverrides,
  });

  let state = buildState();

  const commit = (): void => {
    state = buildState();
    for (const listener of listeners) {
      listener(state);
    }
  };

  // `hasOwnKey` rather than a bracket lookup, so a stored named selection of
  // `__proto__` cannot resolve through the prototype chain of a `userPalettes`
  // that is not (yet, or no longer) null-prototype.
  const isResolvableName = (name: string): boolean =>
    name === 'dark' || name === 'light' || hasOwnKey(userPalettes, name);

  /**
   * The exclusive branches of the second initialization stage. A stored
   * selection wins over the file's `default`, which is an initial value only.
   */
  const selectionAfterLoad = (defaultSelection: AppThemeSelection | null): AppThemeSelection => {
    if (!hasStoredSelection) {
      return defaultSelection ?? selection;
    }
    if (typeof selection === 'string' || isResolvableName(selection.named)) {
      return selection;
    }
    return defaultSelection ?? 'dark';
  };

  // The store lives as long as the application, so the remover is not kept:
  // there is no disposal point in its contract.
  deps.onPrefersDarkChange(() => {
    // The OS preference feeds the `system` selection and nothing else.
    if (selection === 'system') {
      commit();
    }
  });

  return {
    subscribe: (run) => {
      listeners.add(run);
      run(state);
      return () => {
        listeners.delete(run);
      };
    },
    select: (next) => {
      selection = next;
      hasStoredSelection = true;
      try {
        deps.storageWrite(serializeSelection(next));
      } catch {
        // Persisting is best effort: a blocked storage must not keep the
        // selection from taking effect for this run.
      }
      commit();
    },
    setWindowOverride: (windowId, themeName) => {
      // `Object.assign` onto a fresh `Object.create(null)` rather than a
      // spread, so the copy stays null-prototype.
      const next = Object.assign(
        Object.create(null) as Record<string, string | null>,
        windowOverrides
      );
      if (themeName === null) {
        delete next[windowId];
      } else {
        next[windowId] = themeName;
      }
      windowOverrides = next;
      commit();
    },
    loadUserThemes: (result) => {
      const parsed = parseUserThemes(result.themes, result.vendorAssignments, result.defaultTheme);
      userPalettes = parsed.palettes;
      vendorAssignments = parsed.vendorAssignments;
      issues = parsed.issues;
      loadFailure =
        result.failure === undefined || result.failure === 'not_found' ? null : result.failure;
      selection = selectionAfterLoad(parsed.defaultSelection);
      commit();
    },
  };
}

/** The subset of `MediaQueryList` the store needs. */
type MediaQuery = Pick<MediaQueryList, 'matches' | 'addEventListener' | 'removeEventListener'>;

/**
 * Builds {@link ThemeStoreDeps} from the browser globals.
 *
 * Every call is guarded: a WebView that denies `localStorage` or has no
 * `matchMedia` degrades to "no stored selection, light preference" instead of
 * taking the application down.
 *
 * @param g overrides for the globals, for tests and embedders.
 */
export function createBrowserThemeDeps(g?: {
  matchMedia?: (query: string) => MediaQuery;
  localStorage?: Pick<Storage, 'getItem' | 'setItem'>;
}): ThemeStoreDeps {
  const openMediaQuery = (): MediaQuery | null => {
    try {
      const matchMedia = g?.matchMedia ?? globalThis.matchMedia.bind(globalThis);
      return matchMedia(PREFERS_DARK_QUERY);
    } catch {
      // No media query support: the OS preference stays unknown.
      return null;
    }
  };

  const storage = createGuardedLocalStorage(STORAGE_KEY, g?.localStorage);

  return {
    prefersDark: () => openMediaQuery()?.matches ?? false,
    onPrefersDarkChange: (listener) => {
      const query = openMediaQuery();
      if (query === null) {
        return () => {
          // Nothing was registered.
        };
      }
      const handler = (): void => {
        listener();
      };
      query.addEventListener('change', handler);
      return () => {
        query.removeEventListener('change', handler);
      };
    },
    storageRead: () => storage.read(),
    storageWrite: (value) => storage.write(value),
  };
}
