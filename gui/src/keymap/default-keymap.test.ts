/**
 * The default chord table per host platform. The table is written out here in
 * full so that changing a default is a deliberate edit of this file too.
 */
import { describe, expect, it } from 'vitest';

import { validateBinding } from './binding-rules';
import { defaultKeysFor } from './default-keymap';
import type { KeyboardPlatform } from './keyboard-platform';
import { ACTION_IDS, type ActionId } from './key-binding';

const PLATFORMS: readonly KeyboardPlatform[] = ['windows', 'mac', 'linux'];

const WINDOWS_AND_LINUX: Readonly<Record<ActionId, string>> = {
  'focus-next-window': 'Ctrl+Tab',
  'focus-previous-window': 'Ctrl+Shift+Tab',
  'focus-next-attention': 'Ctrl+Shift+a',
  'open-window-form': 'Ctrl+Shift+n',
  'close-window': 'Ctrl+Shift+w',
  'interrupt-window': 'Ctrl+Shift+k',
  'minimize-window': 'Ctrl+Shift+m',
  'toggle-maximize-window': 'Ctrl+Shift+Enter',
  'toggle-sidebar-focus': 'Ctrl+Shift+e',
  'toggle-chat': 'Ctrl+Shift+l',
  'toggle-plans': 'Ctrl+Shift+p',
  'toggle-settings': 'Ctrl+,',
};

const MAC: Readonly<Record<ActionId, string>> = {
  'focus-next-window': 'Ctrl+Tab',
  'focus-previous-window': 'Ctrl+Shift+Tab',
  'focus-next-attention': 'Shift+Meta+a',
  'open-window-form': 'Meta+n',
  'close-window': 'Shift+Meta+w',
  'interrupt-window': 'Meta+.',
  'minimize-window': 'Shift+Meta+m',
  'toggle-maximize-window': 'Shift+Meta+Enter',
  'toggle-sidebar-focus': 'Shift+Meta+e',
  'toggle-chat': 'Shift+Meta+l',
  'toggle-plans': 'Shift+Meta+p',
  'toggle-settings': 'Meta+,',
};

const EXPECTED: Readonly<Record<KeyboardPlatform, Readonly<Record<ActionId, string>>>> = {
  windows: WINDOWS_AND_LINUX,
  linux: WINDOWS_AND_LINUX,
  mac: MAC,
};

describe.each(PLATFORMS)('the default table on %s', (platform) => {
  const table: Readonly<Record<ActionId, string>> = defaultKeysFor(platform);

  it('is the agreed table', () => {
    expect({ ...table }).toEqual({ ...EXPECTED[platform] });
  });

  it('gives every action a chord and names no other action', () => {
    expect(Object.keys(table).sort()).toEqual([...ACTION_IDS].sort());
    for (const action of ACTION_IDS) {
      expect(table[action], action).not.toBe('');
    }
  });

  it('never puts one chord on two actions', () => {
    const chords = ACTION_IDS.map((action) => table[action]);
    expect(new Set(chords).size).toBe(chords.length);
  });

  it('breaks none of the rules a user binding has to obey', () => {
    for (const action of ACTION_IDS) {
      expect(validateBinding(table[action], platform), `${action}: ${table[action]}`).toBeNull();
    }
  });
});

describe('the default tables of different platforms', () => {
  it('use Meta on mac and never on the other platforms', () => {
    const others = Object.values(defaultKeysFor('windows'));
    expect(others.filter((chord) => chord.includes('Meta'))).toEqual([]);
    const mac = Object.values(defaultKeysFor('mac'));
    expect(mac.filter((chord) => chord.includes('Meta')).length).toBeGreaterThan(0);
  });
});
