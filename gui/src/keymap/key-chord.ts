import { chordOf } from './key-binding';
import type { KeyboardPlatform } from './keyboard-platform';

/** The parts of a `KeyboardEvent` that decide which chord was pressed. */
export interface KeyEventLike {
  key: string;
  code: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

const MODIFIER_KEY_NAMES: ReadonlySet<string> = new Set([
  'Control',
  'Shift',
  'Alt',
  'Meta',
  'AltGraph',
  'OS',
]);

// Keys the engine left unnamed (`Process` under an input method, `Unidentified`
// for WebKitGTK's Shift+Tab); the physical key still names them.
const NAMELESS_KEY_NAMES: ReadonlySet<string> = new Set(['Process', 'Unidentified']);
const KEY_OF_CODE_WHEN_NAMELESS: Readonly<Record<string, string>> = {
  Tab: 'Tab',
  Enter: 'Enter',
  Comma: ',',
  Period: '.',
};

/** The fields of a keydown that tell whether the input method owns the key. */
export interface ImeKeyLike {
  isComposing: boolean;
  keyCode: number;
  ctrlKey: boolean;
  metaKey: boolean;
}

/**
 * Whether the input method owns `event`, so no handler may read it as an
 * action. WebKit on macOS reports the Escape that cancels a composition and
 * the Enter that confirms it with `isComposing` already false but keyCode 229;
 * a chord with Ctrl or Meta is the user's even then.
 */
export function isImeKey(event: ImeKeyLike): boolean {
  return event.isComposing || (event.keyCode === 229 && !event.ctrlKey && !event.metaKey);
}

function isAsciiLetter(key: string): boolean {
  return /^[A-Za-z]$/.test(key);
}

function keyNameOf(event: KeyEventLike): string {
  const { key, code } = event;
  if (isAsciiLetter(key)) return key.toLowerCase();
  const physicalLetter = /^Key([A-Z])$/.exec(code);
  if (physicalLetter?.[1] !== undefined) return physicalLetter[1].toLowerCase();
  if (NAMELESS_KEY_NAMES.has(key)) return KEY_OF_CODE_WHEN_NAMELESS[code] ?? key;
  if (key === ' ') return 'Space';
  // `+` is the chord separator, so the key itself is named Plus.
  if (key === '+') return 'Plus';
  return key;
}

/**
 * Builds the canonical chord (`Ctrl+Alt+Shift+Meta+key`) for a key event.
 * Letters follow the character the layout typed and fall back to the physical
 * key only when that character is not an ASCII letter. Returns null while only
 * a modifier key is down; a chord without modifiers is returned as is.
 */
export function chordOfEvent(event: KeyEventLike): string | null {
  if (MODIFIER_KEY_NAMES.has(event.key)) return null;
  return chordOf(
    { ctrl: event.ctrlKey, alt: event.altKey, shift: event.shiftKey, meta: event.metaKey },
    keyNameOf(event)
  );
}

/**
 * Splits a canonical chord into the parts a user reads: modifiers named for
 * the platform, then the key (letters upper-cased). On macOS Cmd comes first.
 */
export function formatChord(chord: string, platform: KeyboardPlatform): string[] {
  const keyStart = chord.lastIndexOf('+') + 1;
  const key = chord.slice(keyStart);
  const names = keyStart === 0 ? [] : chord.slice(0, keyStart - 1).split('+');
  const keyLabel = isAsciiLetter(key) ? key.toUpperCase() : key;
  if (platform === 'mac') {
    const modifiers = new Set(names);
    const labels: string[] = [];
    if (modifiers.has('Meta')) labels.push('Cmd');
    if (modifiers.has('Ctrl')) labels.push('Ctrl');
    if (modifiers.has('Alt')) labels.push('Option');
    if (modifiers.has('Shift')) labels.push('Shift');
    return [...labels, keyLabel];
  }
  return [...names.map((name) => (name === 'Meta' ? 'Win' : name)), keyLabel];
}
