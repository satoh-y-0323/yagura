import { CLIPBOARD_CHORDS } from './clipboard-chords';
import { chordOf, parseChord, type ParsedChord } from './key-binding';
import type { KeyboardPlatform } from './keyboard-platform';
import { isReloadChord } from './reload-keys';
import terminalKeys from './terminal-key-bytes.generated.json';

/** Why a chord cannot be bound to an action. */
export type BindingRejection =
  | 'invalid-chord'
  | 'needs-modifier'
  | 'alt-reaches-terminal'
  | 'types-characters'
  | 'reserved-for-clipboard'
  | 'reserved-by-app-menu'
  | 'reaches-terminal'
  | 'reserved-for-reload';

interface ReservedChord {
  chord: string;
  reason: 'reserved-for-clipboard' | 'reserved-by-app-menu';
}

function clipboardReservations(platform: 'windows' | 'linux'): ReservedChord[] {
  const { copy, paste } = CLIPBOARD_CHORDS[platform];
  return [
    { chord: copy, reason: 'reserved-for-clipboard' },
    { chord: paste, reason: 'reserved-for-clipboard' },
  ];
}

/**
 * Chords no action may take: the terminal's clipboard chords, and on macOS the
 * native app menu's items, which this table is the single place to name.
 */
export const HOST_RESERVED_CHORDS: Readonly<Record<KeyboardPlatform, readonly ReservedChord[]>> = {
  windows: clipboardReservations('windows'),
  linux: clipboardReservations('linux'),
  // The predefined app menu items of the native shell (muda) and the agreed mock.
  mac: [
    { chord: 'Meta+q', reason: 'reserved-by-app-menu' },
    { chord: 'Meta+w', reason: 'reserved-by-app-menu' },
    { chord: 'Meta+h', reason: 'reserved-by-app-menu' },
    { chord: 'Meta+m', reason: 'reserved-by-app-menu' },
    { chord: 'Meta+z', reason: 'reserved-by-app-menu' },
    { chord: 'Meta+x', reason: 'reserved-by-app-menu' },
    { chord: 'Meta+c', reason: 'reserved-by-app-menu' },
    { chord: 'Meta+v', reason: 'reserved-by-app-menu' },
    { chord: 'Meta+a', reason: 'reserved-by-app-menu' },
    { chord: 'Shift+Meta+z', reason: 'reserved-by-app-menu' },
    { chord: 'Alt+Meta+h', reason: 'reserved-by-app-menu' },
    { chord: 'Ctrl+Meta+f', reason: 'reserved-by-app-menu' },
  ],
};

// Key names that type a character although the name is longer than one.
const TYPING_KEY_NAMES: ReadonlySet<string> = new Set(['Plus', 'Space']);

/** Whether a key types a character, as opposed to keys like Tab, F5 and the arrows. */
function typesCharacter(key: string): boolean {
  return TYPING_KEY_NAMES.has(key) || key.length === 1;
}

// What each chord writes to a window, as hex (or `view:up` / `view:down` for a
// move of the view), written by the daemon's tests from its key encoder
// (`crates/yagura-daemon/tests/gui_generated_tables.rs`). A chord the table
// lacks writes nothing.
const BYTES_OF_CHORD: Readonly<Record<string, string>> = terminalKeys.chords;

function writtenBy(parsed: Pick<ParsedChord, 'alt' | 'shift' | 'key'>, ctrl: boolean): string {
  const chord = chordOf({ ctrl, alt: parsed.alt, shift: parsed.shift, meta: false }, parsed.key);
  return BYTES_OF_CHORD[chord] ?? '';
}

/**
 * Whether Ctrl changes what the terminal receives for `parsed`. It adds nothing
 * when the chord writes nothing, or writes what the same key writes without Ctrl
 * and with only some of the other modifiers. A chord with Meta is never written.
 */
function ctrlReachesTerminal(parsed: ParsedChord): boolean {
  if (!parsed.ctrl || parsed.meta) return false;
  const own = writtenBy(parsed, true);
  if (own === '') return false;
  const others = [
    { alt: false, shift: false },
    { alt: true, shift: false },
    { alt: false, shift: true },
    { alt: true, shift: true },
  ].filter((subset) => (!subset.alt || parsed.alt) && (!subset.shift || parsed.shift));
  return !others.some((subset) => writtenBy({ ...subset, key: parsed.key }, false) === own);
}

/**
 * Decides whether `chord` may be bound to an action. Returns why not, or null.
 * The single gate for the default table, the keymap file and the Keyboard tab.
 */
export function validateBinding(
  chord: string,
  platform: KeyboardPlatform
): BindingRejection | null {
  const parsed = parseChord(chord);
  if (parsed === null) return 'invalid-chord';
  if (!parsed.ctrl && !parsed.meta) return parsed.alt ? 'alt-reaches-terminal' : 'needs-modifier';
  if (parsed.ctrl && parsed.alt && typesCharacter(parsed.key)) return 'types-characters';
  const reserved = HOST_RESERVED_CHORDS[platform].find((entry) => entry.chord === chord);
  if (reserved !== undefined) return reserved.reason;
  if (ctrlReachesTerminal(parsed)) return 'reaches-terminal';
  // The key listener swallows these before the dispatcher, so a binding would never fire.
  return isReloadChord(chord, platform) ? 'reserved-for-reload' : null;
}
