import { BUILT_IN_PALETTES, hasOwnKey, isRecordLike, Palette, validatePalette } from './palette';

/** The app theme selections yagura ships; a user theme cannot take one of these names. */
export const BUILT_IN_SELECTIONS = ['dark', 'light', 'system'] as const;

export type BuiltInSelection = (typeof BUILT_IN_SELECTIONS)[number];

export type AppThemeSelection = BuiltInSelection | { named: string };

/** Whether `name` is one of {@link BUILT_IN_SELECTIONS}. */
export function isBuiltInSelection(name: string): name is BuiltInSelection {
  return (BUILT_IN_SELECTIONS as readonly string[]).includes(name);
}

export interface ThemeIssue {
  code: 'invalid_palette' | 'reserved_name' | 'unknown_reference';
  theme?: string;
}

export interface UserThemesParseResult {
  palettes: Record<string, Palette>;
  vendorAssignments: Record<string, string>;
  defaultSelection: AppThemeSelection | null;
  issues: ThemeIssue[];
}

export interface WindowPaletteSource {
  appPalette: Palette;
  userPalettes: Record<string, Palette>;
  vendorAssignments: Record<string, string>;
  windowOverrides: Record<string, string | null>;
}

const BUILTIN_NAMES = new Set<string>(['dark', 'light']);

function isValidThemeName(name: unknown): name is string {
  return typeof name === 'string';
}

// Delegating wrappers: the object-shape check itself lives once, in
// `isRecordLike` (`palette.ts`).
function isValidPaletteObject(value: unknown): value is Record<string, unknown> {
  return isRecordLike(value);
}

function isPalette(value: unknown): value is Palette {
  return isRecordLike(value);
}

function getPaletteIfExists(palettes: Record<string, Palette>, name: string): Palette | undefined {
  const candidate = palettes[name];
  return isPalette(candidate) ? candidate : undefined;
}

/**
 * Parses the raw themes file data into validated palettes and assignments.
 * Returns issues for invalid palettes, reserved names, and
 * unknown references; an empty issue list does not guarantee that all
 * returned selections are resolvable (see `resolveAppPalette` and
 * `resolveWindowPalette`).
 */
export function parseUserThemes(
  themes: unknown,
  vendorAssignments: unknown,
  defaultTheme: unknown
): UserThemesParseResult {
  const result: UserThemesParseResult = {
    // Null-prototype, so a theme/vendor name equal to `__proto__` becomes a
    // plain own key instead of rewriting this object's prototype.
    palettes: Object.create(null) as Record<string, Palette>,
    vendorAssignments: Object.create(null) as Record<string, string>,
    defaultSelection: null,
    issues: [],
  };

  if (isValidPaletteObject(themes)) {
    // No name-type guard: `Object.entries` only ever yields string keys.
    for (const [themeName, paletteValue] of Object.entries(themes)) {
      if (isBuiltInSelection(themeName)) {
        result.issues.push({ code: 'reserved_name', theme: themeName });
        continue;
      }

      const validated = validatePalette(paletteValue);
      if (validated === null) {
        result.issues.push({ code: 'invalid_palette', theme: themeName });
        continue;
      }

      result.palettes[themeName] = validated;
    }
  }

  if (isValidPaletteObject(vendorAssignments)) {
    for (const [vendor, targetName] of Object.entries(vendorAssignments)) {
      if (!isValidThemeName(targetName)) {
        continue;
      }

      // `hasOwnKey` rather than `in`, so a name that exists on
      // `Object.prototype` (e.g. `toString`) is never mistaken for a theme.
      const exists = hasOwnKey(result.palettes, targetName) || BUILTIN_NAMES.has(targetName);
      if (!exists) {
        result.issues.push({ code: 'unknown_reference', theme: targetName });
        continue;
      }

      result.vendorAssignments[vendor] = targetName;
    }
  }

  if (isValidThemeName(defaultTheme)) {
    // A reserved word becomes a mode literal rather than a palette reference.
    if (isBuiltInSelection(defaultTheme)) {
      result.defaultSelection = defaultTheme;
      return result;
    }

    // `hasOwnKey` for the reason given at the vendor assignment check above.
    const exists = hasOwnKey(result.palettes, defaultTheme) || BUILTIN_NAMES.has(defaultTheme);
    if (!exists) {
      result.issues.push({ code: 'unknown_reference', theme: defaultTheme });
      return result;
    }

    result.defaultSelection = { named: defaultTheme };
  }

  return result;
}

/**
 * Resolves the application palette for a given selection. Named references
 * that do not exist fall back to the built-in dark palette. The `system`
 * selection follows the OS preference.
 */
export function resolveAppPalette(
  selection: AppThemeSelection,
  prefersDark: boolean,
  userPalettes: Record<string, Palette>
): Palette {
  if (selection === 'system') {
    return prefersDark ? BUILT_IN_PALETTES.dark : BUILT_IN_PALETTES.light;
  }
  if (typeof selection === 'string') {
    return BUILT_IN_PALETTES[selection];
  }

  const namedTheme = selection.named;

  // `hasOwnKey` rather than `in`, so a name like `__proto__` or `toString`
  // cannot resolve through the prototype chain of `userPalettes`.
  const userPalette = hasOwnKey(userPalettes, namedTheme) ? userPalettes[namedTheme] : undefined;
  if (userPalette !== undefined) {
    return userPalette;
  }

  return namedTheme === 'light' ? BUILT_IN_PALETTES.light : BUILT_IN_PALETTES.dark;
}

/**
 * Resolves the palette for one window using a staged fallback:
 * window override > vendor assignment > application palette. Unknown names
 * are skipped; the fallback to app palette is never null.
 */
export function resolveWindowPalette(
  vendor: string | null,
  override: string | null,
  vendorAssignments: Record<string, string>,
  userPalettes: Record<string, Palette>,
  appPalette: Palette
): Palette {
  // Built on a null-prototype base (`Object.assign`, not a spread, which
  // would restore `Object.prototype`), so a lookup name colliding with an
  // `Object.prototype` member cannot resolve to anything.
  const allPalettes: Record<string, Palette> = Object.assign(
    Object.create(null) as Record<string, Palette>,
    { dark: BUILT_IN_PALETTES.dark, light: BUILT_IN_PALETTES.light },
    userPalettes
  );

  if (override !== null) {
    const palette = getPaletteIfExists(allPalettes, override);
    if (palette !== undefined) {
      return palette;
    }
  }

  // `hasOwnKey` for the reason given in `resolveAppPalette`.
  if (vendor !== null && hasOwnKey(vendorAssignments, vendor)) {
    const vendorTarget = vendorAssignments[vendor];
    if (vendorTarget !== undefined) {
      const palette = getPaletteIfExists(allPalettes, vendorTarget);
      if (palette !== undefined) {
        return palette;
      }
    }
  }

  return appPalette;
}

/**
 * Convenience wrapper that extracts the window override from a state and
 * calls {@link resolveWindowPalette}.
 */
export function resolveWindowPaletteFromState(
  state: WindowPaletteSource,
  vendor: string | null,
  windowId: string
): Palette {
  const override = state.windowOverrides[windowId] ?? null;
  return resolveWindowPalette(
    vendor,
    override,
    state.vendorAssignments,
    state.userPalettes,
    state.appPalette
  );
}
