import { describe, it, expect } from 'vitest';
import { decideRightClick, type RightClickContext } from './terminal-right-click';

const PLAIN: Omit<RightClickContext, 'platform'> = {
  appCapturesMouse: false,
  shiftKey: false,
  hasSelection: false,
};

describe('decideRightClick', () => {
  it('copies the selection on Windows', () => {
    expect(decideRightClick({ ...PLAIN, platform: 'windows', hasSelection: true })).toBe('copy');
  });

  it('pastes on Windows when nothing is selected', () => {
    expect(decideRightClick({ ...PLAIN, platform: 'windows' })).toBe('paste');
  });

  it.each(['mac', 'linux'] as const)('opens the menu on %s', (platform) => {
    expect(decideRightClick({ ...PLAIN, platform })).toBe('menu');
    expect(decideRightClick({ ...PLAIN, platform, hasSelection: true })).toBe('menu');
  });

  it.each(['windows', 'mac', 'linux'] as const)(
    'leaves the click to an application that captures the mouse on %s',
    (platform) => {
      expect(decideRightClick({ ...PLAIN, platform, appCapturesMouse: true })).toBe('pass-to-app');
      expect(
        decideRightClick({ ...PLAIN, platform, appCapturesMouse: true, hasSelection: true })
      ).toBe('pass-to-app');
    }
  );

  it('handles Shift+right-click itself even when the application captures the mouse', () => {
    const captured = { ...PLAIN, appCapturesMouse: true, shiftKey: true };
    expect(decideRightClick({ ...captured, platform: 'windows' })).toBe('paste');
    expect(decideRightClick({ ...captured, platform: 'windows', hasSelection: true })).toBe('copy');
    expect(decideRightClick({ ...captured, platform: 'mac' })).toBe('menu');
    expect(decideRightClick({ ...captured, platform: 'linux' })).toBe('menu');
  });
});
