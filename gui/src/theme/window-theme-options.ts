/**
 * The choices a window's theme select offers, built in one place so every
 * menu that overrides a window's theme lists the same names in the same order.
 */
import { describeThemeName, themes } from '../ui-messages';
import type { BUILT_IN_PALETTES } from './palette';

/** One option of a theme select; `value` is what the selection stores. */
export interface ThemeOption {
  value: string;
  label: string;
}

const BUILT_IN_NAMES: readonly (keyof typeof BUILT_IN_PALETTES)[] = ['dark', 'light'];

/**
 * Following the application comes first (the empty value, which clears the
 * override), then the built-in palettes, then the user's in the order the
 * themes file names them.
 * @param userPalettes the user's palettes, keyed by name
 */
export function windowThemeOptions(userPalettes: Readonly<Record<string, unknown>>): ThemeOption[] {
  return [
    { value: '', label: themes.windowFollowsAppOption },
    ...[...BUILT_IN_NAMES, ...Object.keys(userPalettes)].map((name) => ({
      value: name,
      label: describeThemeName(name),
    })),
  ];
}
