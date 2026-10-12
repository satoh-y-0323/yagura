/**
 * The terminal's own key handling stays an input converter and never judges
 * shortcuts.
 */
import { describe, it, expect } from 'vitest';

import { defaultKeysFor } from './keymap/default-keymap';
import type { KeyboardPlatform } from './keymap/keyboard-platform';
import { ACTION_IDS } from './keymap/key-binding';
import { decideKeyDown, type KeyDownFacts } from './terminal/key-input';

describe('the terminal key handling stays an input converter', () => {
  const KEY_CODES: Readonly<Record<string, number>> = {
    Tab: 9,
    Enter: 13,
    ',': 188,
    '.': 190,
  };

  /** A keydown for a canonical chord, shaped like the browser sends it. */
  function keydownOf(chord: string): KeyDownFacts {
    const parts = chord.split('+');
    const key = parts.pop() ?? '';
    const shiftKey = parts.includes('Shift');
    const letter = /^[a-z]$/.test(key);
    return {
      type: 'keydown',
      key: letter && shiftKey ? key.toUpperCase() : key,
      code: letter ? `Key${key.toUpperCase()}` : '',
      keyCode: letter ? key.toUpperCase().charCodeAt(0) : (KEY_CODES[key] ?? 0),
      ctrlKey: parts.includes('Ctrl'),
      altKey: parts.includes('Alt'),
      shiftKey,
      metaKey: parts.includes('Meta'),
      isComposing: false,
      getModifierState: () => false,
    };
  }

  it.each<KeyboardPlatform>(['windows', 'mac', 'linux'])(
    'never takes a default %s shortcut as a clipboard action',
    (platform) => {
      const keys = defaultKeysFor(platform);
      for (const action of ACTION_IDS) {
        expect(decideKeyDown(keydownOf(keys[action]), platform).kind, keys[action]).not.toBe(
          'clipboard'
        );
      }
    }
  );

  it('still converts Shift+Enter and leaves the Windows Ctrl+V to the native paste', () => {
    expect(decideKeyDown(keydownOf('Shift+Enter'), 'windows')).toEqual({
      kind: 'key',
      input: { key: { named: 'enter' }, mods: { shift: true, alt: false, ctrl: false } },
    });
    expect(decideKeyDown(keydownOf('Ctrl+v'), 'windows')).toEqual({ kind: 'browser' });
  });
});
