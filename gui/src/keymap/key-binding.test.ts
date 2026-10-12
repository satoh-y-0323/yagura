/**
 * Action vocabulary, exact-modifier lookup and chord syntax. Nothing here goes
 * through the DOM: the tests reach the pure layer directly.
 */
import { describe, expect, it } from 'vitest';

import {
  ACTION_IDS,
  parseChord,
  resolveAction,
  validateChord,
  type ActionId,
  type KeyMap,
  type KeyPress,
} from './key-binding';

interface Modifiers {
  ctrl?: boolean;
  alt?: boolean;
  shift?: boolean;
  meta?: boolean;
}

function press(key: string, modifiers: Modifiers = {}): KeyPress {
  return {
    key,
    ctrlKey: modifiers.ctrl ?? false,
    altKey: modifiers.alt ?? false,
    shiftKey: modifiers.shift ?? false,
    metaKey: modifiers.meta ?? false,
  };
}

const CTRL_SHIFT: Modifiers = { ctrl: true, shift: true };

// The rows of the Keyboard tab, top to bottom.
const EXPECTED_ORDER: readonly ActionId[] = [
  'focus-next-window',
  'focus-previous-window',
  'focus-next-attention',
  'open-window-form',
  'close-window',
  'interrupt-window',
  'minimize-window',
  'toggle-maximize-window',
  'toggle-sidebar-focus',
  'toggle-chat',
  'toggle-plans',
  'toggle-settings',
];

describe('ACTION_IDS', () => {
  it('lists the twelve actions in the order of the Keyboard tab rows', () => {
    expect([...ACTION_IDS]).toEqual([...EXPECTED_ORDER]);
  });

  it('holds no action twice', () => {
    expect(new Set(ACTION_IDS).size).toBe(ACTION_IDS.length);
  });
});

describe('resolveAction: a lookup in the keymap it is given', () => {
  it('resolves a press to the action bound to its chord', () => {
    const keymap: KeyMap = new Map<string, ActionId>([
      ['Ctrl+Tab', 'focus-next-window'],
      ['Ctrl+Shift+Tab', 'focus-previous-window'],
    ]);
    expect(resolveAction(press('Tab', { ctrl: true }), keymap)).toBe('focus-next-window');
    expect(resolveAction(press('Tab', CTRL_SHIFT), keymap)).toBe('focus-previous-window');
  });

  it('writes the modifiers of the chord in the canonical order', () => {
    const keymap: KeyMap = new Map<string, ActionId>([['Shift+Meta+w', 'close-window']]);
    expect(resolveAction(press('w', { shift: true, meta: true }), keymap)).toBe('close-window');
  });

  it('uses the keymap it is given instead of any built-in table', () => {
    const custom: KeyMap = new Map<string, ActionId>([['Ctrl+Shift+Tab', 'close-window']]);
    expect(resolveAction(press('Tab', CTRL_SHIFT), custom)).toBe('close-window');
  });

  it('returns null for an unbound chord and for an empty keymap', () => {
    const keymap: KeyMap = new Map<string, ActionId>([['Ctrl+Shift+n', 'open-window-form']]);
    expect(resolveAction(press('q', CTRL_SHIFT), keymap)).toBeNull();
    expect(resolveAction(press('n', CTRL_SHIFT), new Map<string, ActionId>())).toBeNull();
  });

  it('leaves the keymap and the key press unchanged', () => {
    const keymap: KeyMap = new Map<string, ActionId>([['Ctrl+Shift+n', 'open-window-form']]);
    const held: KeyPress = press('n', CTRL_SHIFT);
    resolveAction(held, keymap);
    expect([...keymap.entries()]).toEqual([['Ctrl+Shift+n', 'open-window-form']]);
    expect(held).toEqual(press('n', CTRL_SHIFT));
  });
});

describe('resolveAction: the held modifiers must match the chord exactly', () => {
  const keymap: KeyMap = new Map<string, ActionId>([['Ctrl+Shift+n', 'open-window-form']]);

  it('resolves the chord when exactly its modifiers are held', () => {
    expect(resolveAction(press('n', CTRL_SHIFT), keymap)).toBe('open-window-form');
  });

  it('does not resolve when an extra modifier is held', () => {
    expect(resolveAction(press('n', { ctrl: true, shift: true, alt: true }), keymap)).toBeNull();
    expect(resolveAction(press('n', { ctrl: true, shift: true, meta: true }), keymap)).toBeNull();
  });

  it('does not resolve when a required modifier is missing', () => {
    expect(resolveAction(press('n', { ctrl: true }), keymap)).toBeNull();
    expect(resolveAction(press('n', { shift: true }), keymap)).toBeNull();
  });

  it('resolves digits and symbols like letters', () => {
    const symbols: KeyMap = new Map<string, ActionId>([
      ['Ctrl+Shift+1', 'close-window'],
      ['Ctrl+,', 'toggle-settings'],
    ]);
    expect(resolveAction(press('1', CTRL_SHIFT), symbols)).toBe('close-window');
    expect(resolveAction(press(',', { ctrl: true }), symbols)).toBe('toggle-settings');
  });
});

describe('resolveAction: a press without a modifier is never an action', () => {
  it.each(['n', 'Enter', 'ArrowRight', 'F5', ' '])(
    'resolves nothing for a bare %j even when the map holds it',
    (key) => {
      const hostile = new Map<string, string>([[key, 'close-window']]) as unknown as KeyMap;
      expect(resolveAction(press(key), hostile)).toBeNull();
    }
  );

  it('still resolves the same key once a modifier is held', () => {
    const keymap: KeyMap = new Map<string, ActionId>([['Ctrl+n', 'open-window-form']]);
    expect(resolveAction(press('n', { ctrl: true }), keymap)).toBe('open-window-form');
  });
});

describe('resolveAction: a value outside the vocabulary is never returned', () => {
  it.each(['rm -rf /', 'restart-daemon', '__proto__', 'constructor', ' close-window ', ''])(
    'returns null for the value %j that bypassed validation',
    (value) => {
      const hostile = new Map<string, string>([['Ctrl+Shift+p', value]]) as unknown as KeyMap;
      expect(resolveAction(press('p', CTRL_SHIFT), hostile)).toBeNull();
    }
  );
});

describe('validateChord: canonical form', () => {
  it.each([
    'Ctrl+Shift+a',
    'Ctrl+Alt+Shift+Meta+k',
    'Shift+Meta+w',
    'Ctrl+Tab',
    'Ctrl+Shift+F5',
    'Ctrl+Shift+1',
    'Ctrl+,',
    'Ctrl+Space',
    'Ctrl+Plus',
  ])('returns %s unchanged', (chord) => {
    const canonical: string | null = validateChord(chord);
    expect(canonical).toBe(chord);
  });

  it.each([
    'Alt+Ctrl+n',
    'Shift+Ctrl+n',
    'Meta+Ctrl+Alt+n',
    'Ctrl+Shift+Alt+n',
    'Ctrl+Meta+Shift+n',
    'Ctrl+Ctrl+n',
  ])('rejects %s because its modifiers are out of order or repeated', (chord) => {
    const canonical: string | null = validateChord(chord);
    expect(canonical).toBeNull();
  });

  it.each([
    '',
    '+',
    'Ctrl+',
    'Ctrl+Alt+',
    '+Ctrl+n',
    'Ctrl++n',
    'Ctrl++',
    'Hyper+n',
    'ctrl+alt+n',
    'Ctrl + Alt + n',
    'Ctrl+Alt+N',
    'Ctrl+Alt+ArrowRight+n',
  ])('rejects the uninterpretable chord %j', (chord) => {
    const canonical: string | null = validateChord(chord);
    expect(canonical).toBeNull();
  });

  it.each(['n', 'Enter', 'ArrowRight', 'F5', ' '])('rejects the bare key %j', (chord) => {
    const canonical: string | null = validateChord(chord);
    expect(canonical).toBeNull();
  });
});

describe('validateChord: the space key is spelled Space', () => {
  it('accepts Space as a key name', () => {
    const canonical: string | null = validateChord('Ctrl+Shift+Space');
    expect(canonical).toBe('Ctrl+Shift+Space');
  });

  it.each([' ', '\t'])('rejects a whitespace character %j written as the key itself', (key) => {
    const canonical: string | null = validateChord(`Ctrl+Shift+${key}`);
    expect(canonical).toBeNull();
  });
});

describe('parseChord: what a chord may be made of', () => {
  it.each<[string, string]>([
    ['Ctrl+Shift+ArrowRight', 'a key name that starts with a capital'],
    ['Alt+1', 'a digit'],
    ['Ctrl+]', 'a symbol'],
    ['Shift+é', 'a lower-case letter outside ASCII'],
    ['Ctrl+Meta+Space', 'the key name Space'],
  ])('accepts %s (%s)', (chord) => {
    expect(parseChord(chord)).not.toBeNull();
  });

  it.each<[string, string]>([
    ['', 'an empty chord'],
    ['Ctrl+', 'a missing key'],
    ['Foo+a', 'an unknown modifier'],
    ['Shift+Ctrl+a', 'modifiers out of order'],
    ['Ctrl+Ctrl+a', 'a modifier twice'],
    ['Alt+Ctrl', 'a modifier name as the key'],
    ['Ctrl+Meta', 'a modifier name as the key'],
    ['Ctrl+A', 'a lone capital letter'],
    ['Ctrl+aB', 'a key name that does not start with a capital'],
    ['Ctrl+arrow', 'a lower-case key name'],
    ['Ctrl+ ', 'a space character'],
    ['Ctrl+\t', 'a tab character'],
  ])('rejects %j (%s)', (chord) => {
    expect(parseChord(chord)).toBeNull();
  });

  it('refuses a value that is not a string', () => {
    expect(parseChord(5 as unknown as string)).toBeNull();
    expect(parseChord(null as unknown as string)).toBeNull();
    expect(parseChord(undefined as unknown as string)).toBeNull();
  });

  it('reports exactly the modifiers the chord names', () => {
    expect(parseChord('Ctrl+Alt+x')).toEqual({
      ctrl: true,
      alt: true,
      shift: false,
      meta: false,
      key: 'x',
    });
    expect(parseChord('Shift+Meta+Enter')).toEqual({
      ctrl: false,
      alt: false,
      shift: true,
      meta: true,
      key: 'Enter',
    });
    expect(parseChord('Enter')).toEqual({
      ctrl: false,
      alt: false,
      shift: false,
      meta: false,
      key: 'Enter',
    });
  });
});

describe('resolveAction: Alt', () => {
  it('writes Alt into the chord between Ctrl and Shift', () => {
    const keymap: KeyMap = new Map<string, ActionId>([['Ctrl+Alt+Shift+x', 'toggle-chat']]);

    expect(resolveAction(press('x', { ctrl: true, alt: true, shift: true }), keymap)).toBe(
      'toggle-chat'
    );
  });

  it('resolves an Alt-only chord', () => {
    const keymap: KeyMap = new Map<string, ActionId>([['Alt+x', 'toggle-plans']]);

    expect(resolveAction(press('x', { alt: true }), keymap)).toBe('toggle-plans');
  });
});
