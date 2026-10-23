/**
 * Action vocabulary, exact-modifier lookup and chord syntax. Nothing here goes
 * through the DOM: the tests reach the pure layer directly.
 */
import { describe, expect, it } from 'vitest';

import { ACTION_IDS, parseChord, type ActionId } from './key-binding';

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

describe('parseChord: canonical form', () => {
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
  ])('reads %s', (chord) => {
    expect(parseChord(chord)).not.toBeNull();
  });

  it.each([
    'Alt+Ctrl+n',
    'Shift+Ctrl+n',
    'Meta+Ctrl+Alt+n',
    'Ctrl+Shift+Alt+n',
    'Ctrl+Meta+Shift+n',
    'Ctrl+Ctrl+n',
  ])('rejects %s because its modifiers are out of order or repeated', (chord) => {
    expect(parseChord(chord)).toBeNull();
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
    expect(parseChord(chord)).toBeNull();
  });
});

describe('parseChord: the space key is spelled Space', () => {
  it('accepts Space as a key name', () => {
    expect(parseChord('Ctrl+Shift+Space')).not.toBeNull();
  });

  it.each([' ', '\t'])('rejects a whitespace character %j written as the key itself', (key) => {
    expect(parseChord(`Ctrl+Shift+${key}`)).toBeNull();
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
