import { describe, it, expect } from 'vitest';
import { BUILT_IN_PALETTES, PALETTE_TOKEN_KEYS } from './palette';
import type {
  AppThemeSelection,
  ThemeIssue,
  UserThemesParseResult,
  WindowPaletteSource,
} from './theme-resolve';
import {
  parseUserThemes,
  resolveAppPalette,
  resolveWindowPalette,
  resolveWindowPaletteFromState,
} from './theme-resolve';

// Palette fixtures are derived from PALETTE_TOKEN_KEYS on purpose: the literal list
// of token names stays declared in palette.test.ts only.
const TOKEN_KEYS: string[] = [...PALETTE_TOKEN_KEYS];
const DARK: Record<string, string> = BUILT_IN_PALETTES.dark;
const LIGHT: Record<string, string> = BUILT_IN_PALETTES.light;

function paletteOf(colour: string): Record<string, string> {
  const palette: Record<string, string> = {};
  for (const key of TOKEN_KEYS) {
    palette[key] = colour;
  }
  return palette;
}

function prefixedPaletteOf(colour: string): Record<string, string> {
  const palette: Record<string, string> = {};
  for (const key of TOKEN_KEYS) {
    palette[`--${key}`] = colour;
  }
  return palette;
}

function withoutKey(palette: Record<string, string>, key: string): Record<string, string> {
  const copy: Record<string, string> = { ...palette };
  delete copy[key];
  return copy;
}

const OCEAN: Record<string, string> = paletteOf('#010203');
const SAND: Record<string, string> = paletteOf('#0a0b0c');
const MOSS: Record<string, string> = paletteOf('#141516');

const FIRST_KEY: string = TOKEN_KEYS[0] ?? 'color-background';

const RESERVED_NAMES = ['dark', 'light', 'system'] as const;
const VENDOR_NAMES = ['claude', 'codex', 'gemini', 'grok', 'aider'] as const;

function issueKey(issue: ThemeIssue): string {
  return `${issue.code}:${issue.theme ?? ''}`;
}

function issueKeys(result: UserThemesParseResult): string[] {
  return result.issues.map(issueKey).sort();
}

function stateWith(
  windowOverrides: Record<string, string>,
  vendorAssignments: Record<string, string>
): WindowPaletteSource {
  return {
    appPalette: LIGHT,
    userPalettes: { ocean: OCEAN, sand: SAND },
    vendorAssignments,
    windowOverrides,
  };
}

describe('parseUserThemes - user theme vocabulary', () => {
  it('keeps a valid user theme', () => {
    const result = parseUserThemes({ ocean: OCEAN }, {}, undefined);
    expect(result.palettes).toEqual({ ocean: OCEAN });
  });

  it('reports no issue for a valid user theme', () => {
    const result = parseUserThemes({ ocean: OCEAN }, {}, undefined);
    expect(result.issues).toEqual([]);
  });

  it('excludes a theme that is missing a palette key', () => {
    const result = parseUserThemes({ broken: withoutKey(OCEAN, FIRST_KEY) }, {}, undefined);
    expect(result.palettes).toEqual({});
  });

  it('reports invalid_palette for a theme that is missing a palette key', () => {
    const result = parseUserThemes({ broken: withoutKey(OCEAN, FIRST_KEY) }, {}, undefined);
    expect(result.issues).toEqual([{ code: 'invalid_palette', theme: 'broken' }]);
  });

  it('excludes a theme whose keys carry the CSS custom property prefix', () => {
    const result = parseUserThemes({ broken: prefixedPaletteOf('#010203') }, {}, undefined);
    expect(result.palettes).toEqual({});
  });

  it('reports invalid_palette for a theme whose keys carry the CSS custom property prefix', () => {
    const result = parseUserThemes({ broken: prefixedPaletteOf('#010203') }, {}, undefined);
    expect(result.issues).toEqual([{ code: 'invalid_palette', theme: 'broken' }]);
  });

  it('excludes a theme that adds an extra CSS-prefixed key', () => {
    const candidate: Record<string, string> = { ...OCEAN, [`--${FIRST_KEY}`]: '#010203' };
    const result = parseUserThemes({ broken: candidate }, {}, undefined);
    expect(result.palettes).toEqual({});
  });

  it('excludes a theme that adds an unknown key', () => {
    const candidate: Record<string, string> = { ...OCEAN, 'color-extra': '#010203' };
    const result = parseUserThemes({ broken: candidate }, {}, undefined);
    expect(result.palettes).toEqual({});
  });

  it('excludes a theme whose colour value is not #RRGGBB', () => {
    const candidate: Record<string, string> = { ...OCEAN, [FIRST_KEY]: 'red' };
    const result = parseUserThemes({ broken: candidate }, {}, undefined);
    expect(result.palettes).toEqual({});
  });

  it('reports invalid_palette for a theme whose colour value is not #RRGGBB', () => {
    const candidate: Record<string, string> = { ...OCEAN, [FIRST_KEY]: 'red' };
    const result = parseUserThemes({ broken: candidate }, {}, undefined);
    expect(result.issues).toEqual([{ code: 'invalid_palette', theme: 'broken' }]);
  });

  it('excludes a theme whose table is not an object', () => {
    const result = parseUserThemes({ broken: 'not-a-table' }, {}, undefined);
    expect(result.palettes).toEqual({});
  });

  it('reports invalid_palette for a theme whose table is not an object', () => {
    const result = parseUserThemes({ broken: 'not-a-table' }, {}, undefined);
    expect(result.issues).toEqual([{ code: 'invalid_palette', theme: 'broken' }]);
  });

  it('keeps the remaining themes when one theme is excluded', () => {
    const result = parseUserThemes(
      { ocean: OCEAN, broken: withoutKey(SAND, FIRST_KEY), moss: MOSS },
      {},
      undefined
    );
    expect(result.palettes).toEqual({ ocean: OCEAN, moss: MOSS });
  });

  it.each(RESERVED_NAMES)('excludes the reserved theme name %s', (name) => {
    const result = parseUserThemes({ [name]: OCEAN }, {}, undefined);
    expect(result.palettes).toEqual({});
  });

  it.each(RESERVED_NAMES)('reports reserved_name for the theme name %s', (name) => {
    const result = parseUserThemes({ [name]: OCEAN }, {}, undefined);
    expect(result.issues).toEqual([{ code: 'reserved_name', theme: name }]);
  });

  it('keeps a valid theme declared next to a reserved name', () => {
    const result = parseUserThemes({ dark: OCEAN, ocean: OCEAN }, {}, undefined);
    expect(result.palettes).toEqual({ ocean: OCEAN });
  });

  it.each([null, undefined, 'themes', 42, true])(
    'returns an empty vocabulary when themes is %j',
    (themes) => {
      const result = parseUserThemes(themes, {}, undefined);
      expect(result.palettes).toEqual({});
    }
  );

  it('returns empty results for fully absent input', () => {
    const result = parseUserThemes(undefined, undefined, undefined);
    expect(result).toEqual({
      palettes: {},
      vendorAssignments: {},
      defaultSelection: null,
      issues: [],
    });
  });
});

describe('parseUserThemes - vendor assignments', () => {
  it('keeps an assignment that points to a user theme', () => {
    const result = parseUserThemes({ ocean: OCEAN }, { claude: 'ocean' }, undefined);
    expect(result.vendorAssignments).toEqual({ claude: 'ocean' });
  });

  it.each(RESERVED_NAMES.filter((name) => name !== 'system'))(
    'keeps an assignment that points to the built-in palette %s',
    (name) => {
      const result = parseUserThemes({}, { claude: name }, undefined);
      expect(result.vendorAssignments).toEqual({ claude: name });
    }
  );

  it('reports no issue for an assignment that points to a built-in palette', () => {
    const result = parseUserThemes({}, { claude: 'dark' }, undefined);
    expect(result.issues).toEqual([]);
  });

  it('drops an assignment whose value is system', () => {
    const result = parseUserThemes({ ocean: OCEAN }, { claude: 'system' }, undefined);
    expect(result.vendorAssignments).toEqual({});
  });

  it('reports unknown_reference for an assignment whose value is system', () => {
    const result = parseUserThemes({ ocean: OCEAN }, { claude: 'system' }, undefined);
    expect(result.issues).toEqual([{ code: 'unknown_reference', theme: 'system' }]);
  });

  it('drops an assignment that points to an unknown name', () => {
    const result = parseUserThemes({ ocean: OCEAN }, { claude: 'nope' }, undefined);
    expect(result.vendorAssignments).toEqual({});
  });

  it('reports unknown_reference for an assignment that points to an unknown name', () => {
    const result = parseUserThemes({ ocean: OCEAN }, { claude: 'nope' }, undefined);
    expect(result.issues).toEqual([{ code: 'unknown_reference', theme: 'nope' }]);
  });

  it('drops an assignment that points to an excluded theme', () => {
    const result = parseUserThemes(
      { broken: withoutKey(OCEAN, FIRST_KEY) },
      { claude: 'broken' },
      undefined
    );
    expect(result.vendorAssignments).toEqual({});
  });

  it('reports both the exclusion and the dangling reference of an excluded theme', () => {
    const result = parseUserThemes(
      { broken: withoutKey(OCEAN, FIRST_KEY) },
      { claude: 'broken' },
      undefined
    );
    expect(issueKeys(result)).toEqual(['invalid_palette:broken', 'unknown_reference:broken']);
  });

  it('drops an assignment whose value is not a string', () => {
    const result = parseUserThemes({ ocean: OCEAN }, { claude: 42 }, undefined);
    expect(result.vendorAssignments).toEqual({});
  });

  it('keeps the resolvable assignments when another one is dropped', () => {
    const result = parseUserThemes({ ocean: OCEAN }, { claude: 'ocean', codex: 'nope' }, undefined);
    expect(result.vendorAssignments).toEqual({ claude: 'ocean' });
  });

  it.each([null, undefined, 'claude', 42, true])(
    'returns an empty assignment map when vendorAssignments is %j',
    (assignments) => {
      const result = parseUserThemes({ ocean: OCEAN }, assignments, undefined);
      expect(result.vendorAssignments).toEqual({});
    }
  );

  it('keeps assignments for every vendor name without special-casing', () => {
    const assignments: Record<string, string> = {};
    for (const vendor of VENDOR_NAMES) {
      assignments[vendor] = 'ocean';
    }
    const result = parseUserThemes({ ocean: OCEAN }, assignments, undefined);
    expect(result.vendorAssignments).toEqual(assignments);
  });
});

describe('parseUserThemes - default theme mapping', () => {
  it.each(RESERVED_NAMES)('maps the reserved word %s to the mode literal', (name) => {
    const result = parseUserThemes({ ocean: OCEAN }, {}, name);
    expect(result.defaultSelection).toBe(name);
  });

  it('does not wrap the reserved word dark into a named selection', () => {
    const result = parseUserThemes({ ocean: OCEAN }, {}, 'dark');
    expect(result.defaultSelection).not.toEqual({ named: 'dark' });
  });

  it('maps a user theme name to a named selection', () => {
    const result = parseUserThemes({ ocean: OCEAN }, {}, 'ocean');
    expect(result.defaultSelection).toEqual({ named: 'ocean' });
  });

  it('reports no issue for a resolvable default', () => {
    const result = parseUserThemes({ ocean: OCEAN }, {}, 'ocean');
    expect(result.issues).toEqual([]);
  });

  it.each([null, undefined])('returns null when default_theme is %j', (defaultTheme) => {
    const result = parseUserThemes({ ocean: OCEAN }, {}, defaultTheme);
    expect(result.defaultSelection).toBeNull();
  });

  it('reports no issue when default_theme is absent', () => {
    const result = parseUserThemes({ ocean: OCEAN }, {}, null);
    expect(result.issues).toEqual([]);
  });

  it('returns null for an unknown default_theme', () => {
    const result = parseUserThemes({ ocean: OCEAN }, {}, 'nope');
    expect(result.defaultSelection).toBeNull();
  });

  it('reports unknown_reference for an unknown default_theme', () => {
    const result = parseUserThemes({ ocean: OCEAN }, {}, 'nope');
    expect(result.issues).toEqual([{ code: 'unknown_reference', theme: 'nope' }]);
  });

  it('returns null for a default_theme that points to an excluded theme', () => {
    const result = parseUserThemes({ broken: withoutKey(OCEAN, FIRST_KEY) }, {}, 'broken');
    expect(result.defaultSelection).toBeNull();
  });

  it('reports the dangling reference of an excluded default_theme', () => {
    const result = parseUserThemes({ broken: withoutKey(OCEAN, FIRST_KEY) }, {}, 'broken');
    expect(issueKeys(result)).toEqual(['invalid_palette:broken', 'unknown_reference:broken']);
  });

  it.each([
    ['a number', 42],
    ['a boolean', true],
    ['an object', { named: 'ocean' }],
    ['an array', ['ocean']],
  ])('returns null when default_theme is %s', (_label, defaultTheme) => {
    const result = parseUserThemes({ ocean: OCEAN }, {}, defaultTheme);
    expect(result.defaultSelection).toBeNull();
  });
});

describe('resolveAppPalette', () => {
  it('resolves the dark literal to the built-in dark palette', () => {
    expect(resolveAppPalette('dark', false, { ocean: OCEAN })).toEqual(DARK);
  });

  it('resolves the light literal to the built-in light palette', () => {
    expect(resolveAppPalette('light', true, { ocean: OCEAN })).toEqual(LIGHT);
  });

  it('resolves system to the dark palette when prefersDark is true', () => {
    expect(resolveAppPalette('system', true, {})).toEqual(DARK);
  });

  it('resolves system to the light palette when prefersDark is false', () => {
    expect(resolveAppPalette('system', false, {})).toEqual(LIGHT);
  });

  it('resolves a named user theme', () => {
    const selection: AppThemeSelection = { named: 'ocean' };
    expect(resolveAppPalette(selection, false, { ocean: OCEAN })).toEqual(OCEAN);
  });

  it('falls back to the built-in dark palette for an unknown name', () => {
    const selection: AppThemeSelection = { named: 'nope' };
    expect(resolveAppPalette(selection, false, { ocean: OCEAN })).toEqual(DARK);
  });

  it('falls back to the built-in dark palette when userPalettes is empty', () => {
    const selection: AppThemeSelection = { named: 'ocean' };
    expect(resolveAppPalette(selection, true, {})).toEqual(DARK);
  });

  it('resolves the named form of the built-in dark palette', () => {
    const selection: AppThemeSelection = { named: 'dark' };
    expect(resolveAppPalette(selection, false, { ocean: OCEAN })).toEqual(DARK);
  });

  it('resolves the named form of the built-in light palette', () => {
    const selection: AppThemeSelection = { named: 'light' };
    expect(resolveAppPalette(selection, true, { ocean: OCEAN })).toEqual(LIGHT);
  });
});

describe('resolveWindowPalette', () => {
  it('prefers the window override over the vendor assignment', () => {
    expect(
      resolveWindowPalette(
        'claude',
        'ocean',
        { claude: 'sand' },
        { ocean: OCEAN, sand: SAND },
        DARK
      )
    ).toEqual(OCEAN);
  });

  it('resolves a window override that names a built-in palette', () => {
    expect(
      resolveWindowPalette(
        'claude',
        'light',
        { claude: 'sand' },
        { ocean: OCEAN, sand: SAND },
        DARK
      )
    ).toEqual(LIGHT);
  });

  it('skips an unresolvable override and uses the vendor assignment', () => {
    expect(
      resolveWindowPalette('claude', 'nope', { claude: 'sand' }, { ocean: OCEAN, sand: SAND }, DARK)
    ).toEqual(SAND);
  });

  it('uses the vendor assignment when no override is set', () => {
    expect(
      resolveWindowPalette('claude', null, { claude: 'sand' }, { ocean: OCEAN, sand: SAND }, DARK)
    ).toEqual(SAND);
  });

  it('resolves a vendor assignment that names a built-in palette', () => {
    expect(
      resolveWindowPalette('claude', null, { claude: 'light' }, { ocean: OCEAN }, DARK)
    ).toEqual(LIGHT);
  });

  it('falls back to the app palette when the vendor assignment is unresolvable', () => {
    expect(
      resolveWindowPalette('claude', null, { claude: 'nope' }, { ocean: OCEAN }, DARK)
    ).toEqual(DARK);
  });

  it('skips the vendor stage when vendor is null', () => {
    expect(resolveWindowPalette(null, null, { claude: 'ocean' }, { ocean: OCEAN }, LIGHT)).toEqual(
      LIGHT
    );
  });

  it('keeps the window override when vendor is null', () => {
    expect(
      resolveWindowPalette(null, 'ocean', { claude: 'sand' }, { ocean: OCEAN }, LIGHT)
    ).toEqual(OCEAN);
  });

  it('falls back to the app palette when the vendor has no assignment', () => {
    expect(
      resolveWindowPalette('codex', null, { claude: 'ocean' }, { ocean: OCEAN }, LIGHT)
    ).toEqual(LIGHT);
  });

  it('falls back to the app palette when neither stage applies', () => {
    expect(resolveWindowPalette(null, null, {}, { ocean: OCEAN }, LIGHT)).toEqual(LIGHT);
  });

  it('falls back to the app palette when both references are unresolvable', () => {
    expect(
      resolveWindowPalette('claude', 'nope', { claude: 'gone' }, { ocean: OCEAN }, LIGHT)
    ).toEqual(LIGHT);
  });

  it.each(VENDOR_NAMES)('applies the assignment of the vendor %s the same way', (vendor) => {
    expect(
      resolveWindowPalette(vendor, null, { [vendor]: 'ocean' }, { ocean: OCEAN }, DARK)
    ).toEqual(OCEAN);
  });

  it.each(VENDOR_NAMES)(
    'ignores an assignment that belongs to another vendor than %s',
    (vendor) => {
      expect(
        resolveWindowPalette(vendor, null, { 'other-vendor': 'ocean' }, { ocean: OCEAN }, DARK)
      ).toEqual(DARK);
    }
  );
});

describe('resolveWindowPaletteFromState', () => {
  it('uses the window override when the vendor is also assigned', () => {
    const state = stateWith({ 'win-1': 'ocean' }, { claude: 'sand' });
    expect(resolveWindowPaletteFromState(state, 'claude', 'win-1')).toEqual(OCEAN);
  });

  it('uses the window override when the vendor is null', () => {
    const state = stateWith({ 'win-1': 'ocean' }, { claude: 'sand' });
    expect(resolveWindowPaletteFromState(state, null, 'win-1')).toEqual(OCEAN);
  });

  it('uses the vendor assignment when the window has no override', () => {
    const state = stateWith({}, { claude: 'sand' });
    expect(resolveWindowPaletteFromState(state, 'claude', 'win-1')).toEqual(SAND);
  });

  it('uses the app palette when neither an override nor an assignment applies', () => {
    const state = stateWith({}, {});
    expect(resolveWindowPaletteFromState(state, null, 'win-1')).toEqual(LIGHT);
  });

  it('skips an override that is not in the vocabulary and uses the vendor assignment', () => {
    const state = stateWith({ 'win-1': 'nope' }, { claude: 'sand' });
    expect(resolveWindowPaletteFromState(state, 'claude', 'win-1')).toEqual(SAND);
  });

  it('falls back to the app palette when both references are missing', () => {
    const state = stateWith({ 'win-1': 'nope' }, { claude: 'gone' });
    expect(resolveWindowPaletteFromState(state, 'claude', 'win-1')).toEqual(LIGHT);
  });

  it('resolves an override that names a built-in palette', () => {
    const state = stateWith({ 'win-1': 'dark' }, { claude: 'sand' });
    expect(resolveWindowPaletteFromState(state, 'claude', 'win-1')).toEqual(DARK);
  });

  it('does not apply the override of another window', () => {
    const state = stateWith({ 'win-1': 'ocean' }, {});
    expect(resolveWindowPaletteFromState(state, null, 'win-2')).toEqual(LIGHT);
  });

  it('uses the app palette of the given state', () => {
    const state: WindowPaletteSource = {
      appPalette: MOSS,
      userPalettes: { ocean: OCEAN },
      vendorAssignments: {},
      windowOverrides: {},
    };
    expect(resolveWindowPaletteFromState(state, 'claude', 'win-1')).toEqual(MOSS);
  });
});

// An own `__proto__` key must come from JSON.parse: `{ __proto__: v }` creates no
// own key and would make these tests vacuous.
function themesWithProtoTheme(palette: Record<string, string>): unknown {
  return JSON.parse(`{"__proto__": ${JSON.stringify(palette)}}`);
}

describe('parseUserThemes - __proto__ theme name regression', () => {
  it('(i) does not let a __proto__ theme name pollute the returned palette collection', () => {
    const themes = themesWithProtoTheme(OCEAN) as Record<string, unknown>;
    const injectedPalette = themes['__proto__'];
    const result = parseUserThemes(themes, {}, undefined);

    expect(Object.getPrototypeOf({})).toBe(Object.prototype);
    // `result.palettes` is null-prototype and `__proto__` stays a plain own theme
    // name, so the observation point is only whether the injected palette became
    // the prototype.
    expect(Object.getPrototypeOf(result.palettes)).not.toBe(injectedPalette);
  });

  it('(ii) does not let a poisoned prototype bypass the default-theme choke point', () => {
    const result = parseUserThemes(themesWithProtoTheme(OCEAN), {}, 'color-background');

    expect(result.defaultSelection).toBeNull();
  });
});

describe('parseUserThemes - null-prototype themes input', () => {
  it('(iii) keeps a user theme when themes is a null-prototype object', () => {
    const themes: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
    themes['ocean'] = OCEAN;

    const result = parseUserThemes(themes, {}, undefined);

    expect(result.palettes).toEqual({ ocean: OCEAN });
  });
});

describe('theme names that collide with the language', () => {
  it('does not read a null override as a theme named null', () => {
    expect(resolveWindowPalette(null, null, {}, { null: OCEAN }, LIGHT)).toEqual(LIGHT);
  });

  it('does not read a null vendor as an assignment keyed null', () => {
    expect(resolveWindowPalette(null, null, { null: 'ocean' }, { ocean: OCEAN }, LIGHT)).toEqual(
      LIGHT
    );
  });

  it('skips a vendor assignment whose value is missing', () => {
    const assignments = { claude: undefined } as unknown as Record<string, string>;
    expect(resolveWindowPalette('claude', null, assignments, { ocean: OCEAN }, LIGHT)).toEqual(
      LIGHT
    );
  });

  it('falls back to the built-in palettes for a named theme whose palette is missing', () => {
    const palettes = { ocean: undefined } as unknown as Record<string, Record<string, string>>;
    expect(resolveAppPalette({ named: 'ocean' }, false, palettes)).toEqual(DARK);
  });

  it('does not resolve a named theme through the prototype chain', () => {
    expect(resolveAppPalette({ named: 'toString' }, false, {})).toEqual(DARK);
  });
});

describe('parseUserThemes: a vendor assignment that cannot be a theme name', () => {
  it('records no issue for a value that is not a string', () => {
    const result = parseUserThemes({ ocean: OCEAN }, { claude: 42, codex: null }, undefined);
    expect(result.issues).toEqual([]);
    expect(result.vendorAssignments).toEqual({});
  });

  it('reports the system value as an unknown reference named system', () => {
    const result = parseUserThemes({}, { claude: 'system' }, undefined);
    expect(result.issues).toEqual([{ code: 'unknown_reference', theme: 'system' }]);
  });
});
