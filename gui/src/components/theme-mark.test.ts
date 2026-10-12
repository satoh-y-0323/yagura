/**
 * What the rail's theme control shows for the current app theme: an icon
 * that tells the built-in choices apart, and a tooltip that names the theme.
 * The rail is too narrow to show the name itself.
 */
import { describe, it, expect } from 'vitest';

import { themeMarkOf } from './theme-mark';

const OPTIONS = [
  { value: 'dark', label: 'Dark' },
  { value: 'light', label: 'Light' },
  { value: 'system', label: 'System' },
  { value: 'solarized', label: 'solarized' },
];

describe('themeMarkOf', () => {
  it.each([
    ['dark', 'moon'],
    ['light', 'sun'],
    ['system', 'system'],
    ['solarized', 'palette'],
  ])('shows the %s theme with the %s icon', (value, icon) => {
    expect(themeMarkOf(value, OPTIONS).icon).toBe(icon);
  });

  it('names the selected theme by its option label in the tooltip', () => {
    expect(themeMarkOf('dark', OPTIONS).tooltip).toBe('Theme: Dark');
  });

  it('names a user palette by its own name in the tooltip', () => {
    expect(themeMarkOf('solarized', OPTIONS).tooltip).toBe('Theme: solarized');
  });

  it('falls back to the value itself when no option carries it', () => {
    expect(themeMarkOf('gone', OPTIONS)).toEqual({ icon: 'palette', tooltip: 'Theme: gone' });
  });
});

describe('themeMarkOf: icons', () => {
  it('draws a different icon for each built-in theme and one for every user theme', () => {
    const icons = ['dark', 'light', 'system', 'solarized', 'another'].map(
      (value) => themeMarkOf(value, OPTIONS).icon
    );

    expect(icons).toEqual(['moon', 'sun', 'system', 'palette', 'palette']);
  });
});
