import { describe, expect, it } from 'vitest';
import { validateBinding, type BindingRejection } from './binding-rules';
import { defaultKeysFor } from './default-keymap';
import type { KeyboardPlatform } from './keyboard-platform';
import terminalKeys from './terminal-key-bytes.generated.json';

// The table is written by the daemon's tests from its key encoder
// (`crates/yagura-daemon/tests/gui_generated_tables.rs`): what each chord
// writes to a window, as hex, or `view:up` / `view:down` for a move of the
// view. A chord the table lacks writes nothing.
const BYTES_OF_CHORD: Readonly<Record<string, string>> = terminalKeys.chords;

const PLATFORMS: readonly KeyboardPlatform[] = ['windows', 'mac', 'linux'];

const OTHER_RULES: readonly BindingRejection[] = [
  'reserved-for-clipboard',
  'reserved-by-app-menu',
  'reserved-for-reload',
];

function writtenBy(chord: string): string {
  return BYTES_OF_CHORD[chord] ?? '';
}

function isCharacterKey(key: string): boolean {
  return key.length === 1 || key === 'Plus' || key === 'Space';
}

/** `Ctrl` is the first modifier of a canonical chord. */
function withoutCtrl(modifiers: readonly string[]): string[] {
  return modifiers.filter((name) => name !== 'Ctrl');
}

/** Every chord built from `key` and a subset of `modifiers`, in canonical order. */
function chordsOfSubsets(modifiers: readonly string[], key: string): string[] {
  const chords: string[] = [];
  for (let mask = 0; mask < 1 << modifiers.length; mask++) {
    const held = modifiers.filter((_, index) => (mask & (1 << index)) !== 0);
    chords.push([...held, key].join('+'));
  }
  return chords;
}

/**
 * The rule, stated from the table alone: a chord with Ctrl may be bound when it
 * writes nothing, or when what it writes is what the chord without Ctrl (or
 * with only some of its other modifiers) writes, because then Ctrl adds
 * nothing for the terminal.
 */
function ctrlAddsNothing(chord: string): boolean {
  const names = chord.split('+');
  const key = names[names.length - 1] ?? '';
  const modifiers = withoutCtrl(names.slice(0, -1));
  const own = writtenBy(chord);
  if (own === '') return true;
  return chordsOfSubsets(modifiers, key).some((other) => writtenBy(other) === own);
}

function ctrlChordsInTable(): string[] {
  return Object.keys(BYTES_OF_CHORD).filter((chord) => {
    const names = chord.split('+');
    const key = names[names.length - 1] ?? '';
    const modifiers = names.slice(0, -1);
    if (!modifiers.includes('Ctrl')) return false;
    // Ctrl with Alt on a character key is refused as typing characters, before this rule.
    return !(modifiers.includes('Alt') && isCharacterKey(key));
  });
}

describe('a chord with Ctrl is bindable when Ctrl adds nothing the daemon would write', () => {
  it.each(PLATFORMS)(
    'agrees with the daemon-generated table for every Ctrl chord on %s',
    (platform) => {
      const disagreements: string[] = [];
      for (const chord of ctrlChordsInTable()) {
        const actual = validateBinding(chord, platform);
        if (ctrlAddsNothing(chord)) {
          const fine = actual === null || (actual !== null && OTHER_RULES.includes(actual));
          if (!fine) disagreements.push(`${chord}: ${actual}, expected null`);
        } else if (actual !== 'reaches-terminal') {
          disagreements.push(`${chord}: ${actual}, expected reaches-terminal`);
        }
      }
      expect(disagreements.slice(0, 20)).toEqual([]);
      expect(disagreements.length).toBe(0);
    }
  );

  it('covers the chords the table holds with Ctrl', () => {
    expect(ctrlChordsInTable().length).toBeGreaterThan(200);
  });
});

describe('the default key table', () => {
  it.each(PLATFORMS)('is accepted in full on %s', (platform) => {
    const rejected: string[] = [];
    for (const [action, chord] of Object.entries(defaultKeysFor(platform))) {
      const actual = validateBinding(chord, platform);
      if (actual !== null) rejected.push(`${action} ${chord}: ${actual}`);
    }
    expect(rejected).toEqual([]);
  });
});

describe('Ctrl chords that add information for the terminal', () => {
  it.each([
    'Ctrl+ArrowUp',
    'Ctrl+ArrowDown',
    'Ctrl+ArrowLeft',
    'Ctrl+ArrowRight',
    'Ctrl+Home',
    'Ctrl+End',
    'Ctrl+PageUp',
    'Ctrl+PageDown',
    'Ctrl+Delete',
    'Ctrl+Backspace',
    'Ctrl+Space',
    'Ctrl+F5',
    'Ctrl+a',
    'Ctrl+r',
    'Ctrl+Alt+ArrowUp',
  ])('%s is refused because it reaches the terminal', (chord) => {
    for (const platform of PLATFORMS) {
      expect(validateBinding(chord, platform), `${chord} on ${platform}`).toBe('reaches-terminal');
    }
  });
});

describe('Ctrl chords that add nothing for the terminal', () => {
  it.each([
    'Ctrl+Enter',
    'Ctrl+Shift+Enter',
    'Ctrl+Alt+Enter',
    'Ctrl+Tab',
    'Ctrl+Shift+Tab',
    'Ctrl+Escape',
    'Ctrl+Insert',
    'Ctrl+Shift+a',
    'Ctrl+,',
  ])('%s is accepted', (chord) => {
    for (const platform of PLATFORMS) {
      expect(validateBinding(chord, platform), `${chord} on ${platform}`).toBeNull();
    }
  });
});

describe('Enter with Shift and with Ctrl+Shift', () => {
  it('is judged from what the daemon writes: Shift+Enter is ESC CR, Ctrl+Shift+Enter is CR', () => {
    expect(BYTES_OF_CHORD['Shift+Enter']).toBe('1b0d');
    expect(BYTES_OF_CHORD['Ctrl+Shift+Enter']).toBe('0d');
    expect(BYTES_OF_CHORD['Enter']).toBe('0d');
  });

  it('accepts Ctrl+Shift+Enter because Ctrl adds nothing to the Enter it writes', () => {
    for (const platform of PLATFORMS) {
      expect(validateBinding('Ctrl+Shift+Enter', platform)).toBeNull();
    }
  });
});
