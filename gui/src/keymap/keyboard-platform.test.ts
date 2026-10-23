import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  hostKeyboardPlatform,
  keyboardPlatformOf,
  type KeyboardPlatform,
} from './keyboard-platform';

describe('keyboardPlatformOf', () => {
  it.each<[string, KeyboardPlatform]>([
    ['Win32', 'windows'],
    ['Win64', 'windows'],
    ['MacIntel', 'mac'],
    ['iPhone', 'mac'],
    ['iPad', 'mac'],
    ['iPhone Simulator', 'mac'],
    ['iPad Pro', 'mac'],
    ['Linux (iPhone)', 'linux'],
    ['Linux (iPad)', 'linux'],
    ['Linux x86_64', 'linux'],
    ['Linux aarch64', 'linux'],
    ['', 'linux'],
  ])('reads %j as %s', (navigatorPlatform, expected) => {
    const platform: KeyboardPlatform = keyboardPlatformOf(navigatorPlatform);
    expect(platform).toBe(expected);
  });

  it('matches only at the start of the string, so a platform merely mentioning Mac is not mac', () => {
    const platform: KeyboardPlatform = keyboardPlatformOf('Linux (not MacIntel)');
    expect(platform).toBe('linux');
  });
});

describe('hostKeyboardPlatform', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('reads the host from navigator.platform', () => {
    vi.stubGlobal('navigator', { platform: 'MacIntel' });
    const mac: KeyboardPlatform = hostKeyboardPlatform();
    expect(mac).toBe('mac');

    vi.stubGlobal('navigator', { platform: 'Win32' });
    const windows: KeyboardPlatform = hostKeyboardPlatform();
    expect(windows).toBe('windows');
  });

  it('falls back to linux when there is no navigator, so the module still loads outside a browser', () => {
    vi.stubGlobal('navigator', undefined);
    const platform: KeyboardPlatform = hostKeyboardPlatform();
    expect(platform).toBe('linux');
  });
});

describe('hostKeyboardPlatform without a navigator', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('is linux when there is no navigator', () => {
    vi.stubGlobal('navigator', undefined);
    const platform: KeyboardPlatform = hostKeyboardPlatform();
    expect(platform).toBe('linux');
  });
});
