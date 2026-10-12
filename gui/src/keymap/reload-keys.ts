/**
 * The keys that make the WebView reload the GUI. They are cancelled in the
 * page itself so the GUI process needs no OS-specific code to keep them off.
 */

import { chordOfEvent, type KeyEventLike } from './key-chord';
import type { KeyboardPlatform } from './keyboard-platform';

const NON_MAC_RELOAD_CHORDS: ReadonlySet<string> = new Set([
  'F5',
  'Shift+F5',
  'Ctrl+F5',
  'Ctrl+r',
  'Ctrl+Shift+r',
]);

// Ctrl+R on macOS is the shell's history search, not a reload.
const MAC_RELOAD_CHORDS: ReadonlySet<string> = new Set(['Meta+r', 'Shift+Meta+r']);

const RELOAD_CHORDS: Readonly<Record<KeyboardPlatform, ReadonlySet<string>>> = {
  windows: NON_MAC_RELOAD_CHORDS,
  linux: NON_MAC_RELOAD_CHORDS,
  mac: MAC_RELOAD_CHORDS,
};

/**
 * Whether the canonical `chord` reloads the page on `platform`. The table is
 * read only through this function, so binding rules and the key listener agree.
 */
export function isReloadChord(chord: string, platform: KeyboardPlatform): boolean {
  return RELOAD_CHORDS[platform].has(chord);
}

/**
 * Whether `press` is a key that reloads the page on `platform`. The chord is
 * resolved as `chordOfEvent` does, so the letter is found under CapsLock, an
 * input method or a non-Latin layout.
 */
export function isReloadKey(press: KeyEventLike, platform: KeyboardPlatform): boolean {
  const chord = chordOfEvent(press);
  return chord !== null && isReloadChord(chord, platform);
}
