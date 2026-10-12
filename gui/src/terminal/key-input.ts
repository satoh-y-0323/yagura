/**
 * Turns a keydown into what a window receives: a key message the daemon
 * encodes with the window's modes, a clipboard action of the UI, or nothing
 * (the browser's default, which types characters through the input event).
 * Keys and characters are split the way xterm.js 6.0.0 splits them, so a
 * window receives what xterm.js's key encoding would send.
 */
import { CLIPBOARD_CHORDS } from '../keymap/clipboard-chords';
import { chordOfEvent, isImeKey } from '../keymap/key-chord';
import type { KeyboardPlatform } from '../keymap/keyboard-platform';

/** The fields of a DOM `KeyboardEvent` the decision reads. */
export interface KeyDownFacts {
  type: string;
  key: string;
  code: string;
  keyCode: number;
  shiftKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  isComposing: boolean;
  getModifierState(key: string): boolean;
}

/** The modifiers held (Meta never reaches a window). */
export interface Modifiers {
  shift: boolean;
  alt: boolean;
  ctrl: boolean;
}

/** A key that prints nothing by itself, as the wire names it. */
export type NamedKey =
  | 'enter'
  | 'tab'
  | 'backspace'
  | 'escape'
  | 'arrow_up'
  | 'arrow_down'
  | 'arrow_left'
  | 'arrow_right'
  | 'home'
  | 'end'
  | 'page_up'
  | 'page_down'
  | 'insert'
  | 'delete'
  | 'f1'
  | 'f2'
  | 'f3'
  | 'f4'
  | 'f5'
  | 'f6'
  | 'f7'
  | 'f8'
  | 'f9'
  | 'f10'
  | 'f11'
  | 'f12';

/** One key press for the wire (`ClientMessage::Key`). */
export interface KeyInput {
  key: { named: NamedKey } | { char: string };
  mods: Modifiers;
}

/** What a keydown does. */
export type KeyDecision =
  | { kind: 'key'; input: KeyInput }
  | { kind: 'clipboard'; action: 'copy' | 'paste' }
  | { kind: 'browser' };

const NAMED_KEYS: Readonly<Record<string, NamedKey>> = {
  Enter: 'enter',
  Tab: 'tab',
  Backspace: 'backspace',
  Escape: 'escape',
  ArrowUp: 'arrow_up',
  ArrowDown: 'arrow_down',
  ArrowLeft: 'arrow_left',
  ArrowRight: 'arrow_right',
  Home: 'home',
  End: 'end',
  PageUp: 'page_up',
  PageDown: 'page_down',
  Insert: 'insert',
  Delete: 'delete',
  F1: 'f1',
  F2: 'f2',
  F3: 'f3',
  F4: 'f4',
  F5: 'f5',
  F6: 'f6',
  F7: 'f7',
  F8: 'f8',
  F9: 'f9',
  F10: 'f10',
  F11: 'f11',
  F12: 'f12',
};

const MODIFIER_KEYS: ReadonlySet<string> = new Set(['Shift', 'Control', 'Alt', 'Meta', 'AltGraph']);

/** Keys the engine left unnamed; the physical key still names them. */
const NAMELESS_KEYS: ReadonlySet<string> = new Set(['Unidentified', 'Process']);

const KEY_CODE_V = 86;
const KEY_CODE_A = 65;
const KEY_CODE_Z = 90;
/** xterm.js treats only keys past this code as able to type a third-level character. */
const LAST_NON_PRINTING_KEY_CODE = 47;

const BROWSER: KeyDecision = { kind: 'browser' };

/** Decides what a keydown does; see the module comment. */
export function decideKeyDown(event: KeyDownFacts, platform: KeyboardPlatform): KeyDecision {
  if (event.type !== 'keydown' || isImeKey(event) || MODIFIER_KEYS.has(event.key)) {
    return BROWSER;
  }
  // Left to the browser so its own paste event fires.
  if (platform === 'windows' && isPlainCtrl(event) && event.keyCode === KEY_CODE_V) {
    return BROWSER;
  }
  const clipboard = clipboardActionOf(event, platform);
  if (clipboard !== null) {
    return { kind: 'clipboard', action: clipboard };
  }
  if (event.metaKey || isThirdLevelShift(event, platform)) {
    return BROWSER;
  }
  const mods = { shift: event.shiftKey, alt: event.altKey, ctrl: event.ctrlKey };
  const named = namedKeyOf(event);
  if (named !== null) {
    return { kind: 'key', input: { key: { named }, mods } };
  }
  if (!event.ctrlKey && !event.altKey) {
    return BROWSER;
  }
  const char = chordCharacterOf(event);
  return char === null ? BROWSER : { kind: 'key', input: { key: { char }, mods } };
}

/**
 * The text an `input` event typed, or `null` when it typed none the window
 * should receive: composition in progress, deletions and pastes (pastes
 * arrive through the paste event).
 */
export function textOfInputEvent(event: {
  inputType: string;
  data: string | null;
  isComposing: boolean;
}): string | null {
  if (event.inputType !== 'insertText' || event.isComposing) {
    return null;
  }
  return event.data === null || event.data === '' ? null : event.data;
}

function isPlainCtrl(event: KeyDownFacts): boolean {
  return event.ctrlKey && !event.shiftKey && !event.altKey && !event.metaKey;
}

/** macOS copies and pastes through its app menu's copy and paste events. */
function clipboardActionOf(
  event: KeyDownFacts,
  platform: KeyboardPlatform
): 'copy' | 'paste' | null {
  if (platform === 'mac') return null;
  const chord = chordOfEvent(event);
  const chords = CLIPBOARD_CHORDS[platform];
  if (chord === chords.copy) return 'copy';
  if (chord === chords.paste) return 'paste';
  return null;
}

/** xterm.js `_isThirdLevelShift` with Option not treated as Meta. */
function isThirdLevelShift(event: KeyDownFacts, platform: KeyboardPlatform): boolean {
  const isThirdLevel =
    (platform === 'mac' && event.altKey && !event.ctrlKey && !event.metaKey) ||
    (platform === 'windows' && event.altKey && event.ctrlKey && !event.metaKey) ||
    (platform === 'windows' && event.getModifierState('AltGraph'));
  return isThirdLevel && (event.keyCode === 0 || event.keyCode > LAST_NON_PRINTING_KEY_CODE);
}

function namedKeyOf(event: KeyDownFacts): NamedKey | null {
  const name = NAMELESS_KEYS.has(event.key) ? event.code : event.key;
  return NAMED_KEYS[name] ?? null;
}

/**
 * The character of a Ctrl or Alt chord. A letter key is read from its key
 * code, as xterm.js does, so the chord does not depend on the layout.
 */
function chordCharacterOf(event: KeyDownFacts): string | null {
  if (event.keyCode >= KEY_CODE_A && event.keyCode <= KEY_CODE_Z) {
    const letter = String.fromCharCode(event.keyCode);
    return event.shiftKey ? letter : letter.toLowerCase();
  }
  return Array.from(event.key).length === 1 ? event.key : null;
}
