import { describe, expect, it } from 'vitest';
import { HOST_RESERVED_CHORDS, validateBinding, type BindingRejection } from './binding-rules';
import type { KeyboardPlatform } from './keyboard-platform';

// US layout: key code, unshifted character, shifted character.
const US_CHARACTER_KEYS: readonly (readonly [number, string, string])[] = [
  [48, '0', ')'],
  [49, '1', '!'],
  [50, '2', '@'],
  [51, '3', '#'],
  [52, '4', '$'],
  [53, '5', '%'],
  [54, '6', '^'],
  [55, '7', '&'],
  [56, '8', '*'],
  [57, '9', '('],
  [186, ';', ':'],
  [187, '=', '+'],
  [188, ',', '<'],
  [189, '-', '_'],
  [190, '.', '>'],
  [191, '/', '?'],
  [192, '`', '~'],
  [219, '[', '{'],
  [220, '\\', '|'],
  [221, ']', '}'],
  [222, "'", '"'],
];

const NAMED_KEY_CODES: Readonly<Record<string, number>> = {
  Backspace: 8,
  Tab: 9,
  Enter: 13,
  Escape: 27,
  Space: 32,
  PageUp: 33,
  PageDown: 34,
  End: 35,
  Home: 36,
  ArrowLeft: 37,
  ArrowUp: 38,
  ArrowRight: 39,
  ArrowDown: 40,
  Insert: 45,
  Delete: 46,
  F1: 112,
  F2: 113,
  F3: 114,
  F4: 115,
  F5: 116,
  F6: 117,
  F7: 118,
  F8: 119,
  F9: 120,
  F10: 121,
  F11: 122,
  F12: 123,
};

function keyNameOfCharacter(character: string): string {
  return character === '+' ? 'Plus' : character;
}

const LETTERS: readonly string[] = Array.from({ length: 26 }, (_, i) =>
  String.fromCharCode('a'.charCodeAt(0) + i)
);
const CHARACTER_KEY_NAMES: readonly string[] = US_CHARACTER_KEYS.flatMap(([, base, shifted]) => [
  keyNameOfCharacter(base),
  keyNameOfCharacter(shifted),
]);
const TYPING_KEY_NAMES: readonly string[] = [...LETTERS, ...CHARACTER_KEY_NAMES];
const DOMAIN_KEY_NAMES: readonly string[] = [...TYPING_KEY_NAMES, ...Object.keys(NAMED_KEY_CODES)];

interface ChordParts {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
  key: string;
}

function chordOf(parts: ChordParts): string {
  const names: string[] = [];
  if (parts.ctrl) names.push('Ctrl');
  if (parts.alt) names.push('Alt');
  if (parts.shift) names.push('Shift');
  if (parts.meta) names.push('Meta');
  names.push(parts.key);
  return names.join('+');
}

function allChordParts(): ChordParts[] {
  const result: ChordParts[] = [];
  for (let mask = 0; mask < 16; mask++) {
    for (const key of DOMAIN_KEY_NAMES) {
      result.push({
        ctrl: (mask & 4) !== 0,
        alt: (mask & 2) !== 0,
        shift: (mask & 1) !== 0,
        meta: (mask & 8) !== 0,
        key,
      });
    }
  }
  return result;
}

const ALL_PLATFORMS: readonly KeyboardPlatform[] = ['windows', 'mac', 'linux'];

describe('validateBinding over the whole key domain', () => {
  it.each(ALL_PLATFORMS)(
    'rejects Ctrl+Alt with every character key, whatever else is held, on %s',
    (platform: KeyboardPlatform) => {
      const mismatches: string[] = [];
      for (const parts of allChordParts()) {
        if (!(parts.ctrl && parts.alt && TYPING_KEY_NAMES.includes(parts.key))) continue;
        const chord = chordOf(parts);
        const actual: BindingRejection | null = validateBinding(chord, platform);
        if (actual !== 'types-characters') mismatches.push(`${chord}: ${actual}`);
      }
      expect(mismatches.slice(0, 20)).toEqual([]);
      expect(mismatches.length).toBe(0);
    }
  );

  it.each(ALL_PLATFORMS)(
    'rejects every chord without Ctrl or Meta with the reason the user can act on, on %s',
    (platform: KeyboardPlatform) => {
      const mismatches: string[] = [];
      for (const parts of allChordParts()) {
        if (parts.ctrl || parts.meta) continue;
        const chord = chordOf(parts);
        const expected: BindingRejection = parts.alt ? 'alt-reaches-terminal' : 'needs-modifier';
        const actual: BindingRejection | null = validateBinding(chord, platform);
        if (actual !== expected) mismatches.push(`${chord}: ${actual} != ${expected}`);
      }
      expect(mismatches.slice(0, 20)).toEqual([]);
      expect(mismatches.length).toBe(0);
    }
  );

  it('keeps each reserved chord canonical so the lookup cannot miss it', () => {
    for (const platform of ALL_PLATFORMS) {
      for (const entry of HOST_RESERVED_CHORDS[platform]) {
        const rejection: BindingRejection | null = validateBinding(entry.chord, platform);
        expect(rejection, `${entry.chord} on ${platform}`).toBe(entry.reason);
      }
    }
  });
});
