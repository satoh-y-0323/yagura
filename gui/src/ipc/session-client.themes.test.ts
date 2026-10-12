/**
 * TS-boundary contract for `invokeLoadThemes` (the Rust side is tested in
 * `themes.rs`):
 *
 * - the wire shape is snake_case, matching `ThemesLoadResult`'s serde output,
 *   and is narrowed into the camelCase `LoadThemesResult` the app consumes.
 * - `default_theme` absent narrows to `defaultTheme: null`.
 * - `failure` absent narrows to no `failure` property at all, never an
 *   explicit `undefined`.
 * - an unrecognized value, non-object top level or non-string leaf throws
 *   `BoundaryError` rather than being handed on with the wrong shape.
 *
 * `@tauri-apps/api/core` is mocked, so every assertion is about the calls this
 * module makes.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

import { BoundaryError } from './session-boundary';
import { invokeLoadThemes, type LoadThemesResult, type ThemesLoadFailure } from './session-client';

const DARK_THEME_WIRE: Record<string, string> = {
  'color-bg-primary': '#000000',
  'color-text-primary': '#ffffff',
};

beforeEach(() => {
  invokeMock.mockReset();
});

describe('invokeLoadThemes', () => {
  it('calls load_themes with no arguments', async () => {
    invokeMock.mockResolvedValueOnce({
      themes: {},
      vendor_assignments: {},
    });

    await invokeLoadThemes();

    expect(invokeMock).toHaveBeenCalledWith('load_themes');
  });

  it('narrows a full normal-shape reply into the camelCase result', async () => {
    invokeMock.mockResolvedValueOnce({
      themes: { dark: DARK_THEME_WIRE, custom: { 'color-bg-primary': '#123456' } },
      vendor_assignments: { claude: 'dark', codex: 'custom' },
      default_theme: 'dark',
    });

    const result: LoadThemesResult = await invokeLoadThemes();

    expect(result).toEqual({
      themes: { dark: DARK_THEME_WIRE, custom: { 'color-bg-primary': '#123456' } },
      vendorAssignments: { claude: 'dark', codex: 'custom' },
      defaultTheme: 'dark',
    });
  });

  it('narrows a missing default_theme to defaultTheme: null', async () => {
    invokeMock.mockResolvedValueOnce({
      themes: {},
      vendor_assignments: {},
    });

    const result = await invokeLoadThemes();

    expect(result.defaultTheme).toBeNull();
  });

  it('omits the failure property entirely when the wire reply omits it', async () => {
    invokeMock.mockResolvedValueOnce({
      themes: {},
      vendor_assignments: {},
    });

    const result = await invokeLoadThemes();

    expect('failure' in result).toBe(false);
  });

  it.each([['not_found'], ['read_failed'], ['invalid_toml'], ['too_large']] as const)(
    'narrows failure "%s" through unchanged with themes/assignments intact',
    async (failure: ThemesLoadFailure) => {
      invokeMock.mockResolvedValueOnce({
        themes: {},
        vendor_assignments: {},
        failure,
      });

      const result = await invokeLoadThemes();

      const expected: LoadThemesResult = {
        themes: {},
        vendorAssignments: {},
        defaultTheme: null,
        failure,
      };
      expect(result).toEqual(expected);
    }
  );

  it.each([[null], [undefined], [42], ['a string'], [[]]])(
    'rejects with BoundaryError when the top-level reply is not an object (%p)',
    async (value) => {
      invokeMock.mockResolvedValueOnce(value);

      await expect(invokeLoadThemes()).rejects.toThrow(BoundaryError);
    }
  );

  it('rejects with BoundaryError when failure is an unrecognized value', async () => {
    invokeMock.mockResolvedValueOnce({
      themes: {},
      vendor_assignments: {},
      failure: 'disk_on_fire',
    });

    await expect(invokeLoadThemes()).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when default_theme is present but not a string', async () => {
    invokeMock.mockResolvedValueOnce({
      themes: {},
      vendor_assignments: {},
      default_theme: 7,
    });

    await expect(invokeLoadThemes()).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when a theme entry is not an object', async () => {
    invokeMock.mockResolvedValueOnce({
      themes: { dark: 'not-an-object' },
      vendor_assignments: {},
    });

    await expect(invokeLoadThemes()).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when a theme token value is not a string', async () => {
    invokeMock.mockResolvedValueOnce({
      themes: { dark: { 'color-bg-primary': 12345 } },
      vendor_assignments: {},
    });

    await expect(invokeLoadThemes()).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when vendor_assignments is not an object', async () => {
    invokeMock.mockResolvedValueOnce({
      themes: {},
      vendor_assignments: 'dark',
    });

    await expect(invokeLoadThemes()).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when a vendor_assignments value is not a string', async () => {
    invokeMock.mockResolvedValueOnce({
      themes: {},
      vendor_assignments: { claude: 42 },
    });

    await expect(invokeLoadThemes()).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when themes is missing entirely', async () => {
    invokeMock.mockResolvedValueOnce({
      vendor_assignments: {},
    });

    await expect(invokeLoadThemes()).rejects.toThrow(BoundaryError);
  });

  it('rejects with BoundaryError when vendor_assignments is missing entirely', async () => {
    invokeMock.mockResolvedValueOnce({
      themes: {},
    });

    await expect(invokeLoadThemes()).rejects.toThrow(BoundaryError);
  });
});

// The fixtures below must build their own `__proto__` key with JSON.parse:
// `{ __proto__: v }` creates no own key and would make the tests vacuous.
describe('invokeLoadThemes - __proto__ key regression', () => {
  it('(v) does not let a __proto__ theme name on the wire pollute the narrowed themes object', async () => {
    invokeMock.mockResolvedValueOnce({
      themes: JSON.parse(`{"__proto__": ${JSON.stringify(DARK_THEME_WIRE)}}`) as unknown as Record<
        string,
        unknown
      >,
      vendor_assignments: {},
    });

    const result = await invokeLoadThemes();

    // Narrowing copies no field by reference, so nothing survives it to
    // compare by identity; the injected content is compared structurally.
    expect(Object.getPrototypeOf(result.themes)).not.toEqual(DARK_THEME_WIRE);
    expect(result.themes['color-bg-primary']).toBeUndefined();
  });

  it('(v-vendor, no-op fixation) leaves vendor_assignments unpolluted for a __proto__ vendor key holding a string value (the right-hand side is a primitive, so the __proto__ setter is a no-op; green before the fix by design)', async () => {
    invokeMock.mockResolvedValueOnce({
      themes: {},
      vendor_assignments: JSON.parse('{"__proto__": "dark"}') as unknown as Record<string, unknown>,
    });

    const result = await invokeLoadThemes();

    expect(Object.getPrototypeOf(result.vendorAssignments)).toBe(Object.prototype);
    expect(Object.prototype.hasOwnProperty.call(result.vendorAssignments, '__proto__')).toBe(false);
  });
});
