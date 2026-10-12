import { describe, it, expect } from 'vitest';
import { appIsHidden, effectiveVisibility } from './effective-visibility';

describe('appIsHidden', () => {
  it.each([
    [false, 'visible', false],
    [true, 'visible', true],
    [false, 'hidden', true],
    [true, 'hidden', true],
  ] as const)(
    'with the main window minimized=%s and the document %s the app hidden is %s',
    (isMinimized, visibilityState, expected) => {
      expect(appIsHidden({ isMinimized, visibilityState })).toBe(expected);
    }
  );
});

describe('effectiveVisibility', () => {
  it.each([
    [true, false, true],
    [true, true, false],
    [false, false, false],
    [false, true, false],
  ] as const)(
    'with the pane visible=%s and the app hidden=%s the window is shown=%s',
    (paneVisible, appHidden, expected) => {
      expect(effectiveVisibility(paneVisible, appHidden)).toBe(expected);
    }
  );
});
