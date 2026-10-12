/**
 * What the rail's theme control shows for the current app theme. Pure so
 * `AppRail.svelte` has no branching logic of its own.
 */
import { isBuiltInSelection, type BuiltInSelection } from '../theme/theme-resolve';
import { themes } from '../ui-messages';

/** The icon drawn for a theme: one per built-in selection, one for every user theme. */
export type ThemeIcon = 'moon' | 'sun' | 'system' | 'palette';

/** The rail's mark for the current app theme. */
export interface ThemeMark {
  icon: ThemeIcon;
  /** The tooltip naming the theme, since the rail is too narrow to show the name. */
  tooltip: string;
}

// Keyed by the built-in type, so a built-in added without an icon fails to compile.
const BUILT_IN_ICONS: Readonly<Record<BuiltInSelection, ThemeIcon>> = {
  dark: 'moon',
  light: 'sun',
  system: 'system',
};

/**
 * The mark for the app theme `value`, named by the label of the option that
 * carries it, or by `value` itself when no option does.
 * @param value the selected app theme, as the rail's select holds it
 * @param options the rail's theme options
 */
export function themeMarkOf(
  value: string,
  options: readonly { value: string; label: string }[]
): ThemeMark {
  const label = options.find((option) => option.value === value)?.label ?? value;
  return {
    icon: isBuiltInSelection(value) ? BUILT_IN_ICONS[value] : 'palette',
    tooltip: themes.appTooltip(label),
  };
}
