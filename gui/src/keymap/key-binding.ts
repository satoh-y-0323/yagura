/**
 * The action vocabulary, chord syntax and exact-modifier lookup. Which chord an
 * action gets is decided by `keymap-model.ts`; this module only names and parses.
 */

export type ActionId =
  | 'focus-next-window'
  | 'focus-previous-window'
  | 'focus-next-attention'
  | 'open-window-form'
  | 'close-window'
  | 'interrupt-window'
  | 'minimize-window'
  | 'toggle-maximize-window'
  | 'toggle-sidebar-focus'
  | 'toggle-chat'
  | 'toggle-plans'
  | 'toggle-settings';

export interface KeyPress {
  key: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

export type KeyMap = Map<string, ActionId>;

/**
 * Every action, in the order of the Keyboard tab rows. Combining a keymap walks
 * this order, so it also decides which of two overrides on one chord wins.
 */
export const ACTION_IDS: readonly ActionId[] = [
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

/**
 * Resolve a key press to an action using the given keymap.
 *
 * The modifiers (ctrl, alt, shift, meta) must match the chord exactly:
 * if the chord requires Ctrl+Alt, then exactly those two must be held.
 * Extra or missing modifiers result in null.
 *
 * @param keyPress - The key press to resolve
 * @param keymap - The keymap to use for resolution
 * @returns The action ID if the key press matches a binding, null otherwise
 */
export function resolveAction(keyPress: KeyPress, keymap: KeyMap): ActionId | null {
  if (!keyPress.ctrlKey && !keyPress.altKey && !keyPress.shiftKey && !keyPress.metaKey) {
    return null;
  }

  const chord = chordOf(
    {
      ctrl: keyPress.ctrlKey,
      alt: keyPress.altKey,
      shift: keyPress.shiftKey,
      meta: keyPress.metaKey,
    },
    keyPress.key
  );

  const action = keymap.get(chord);

  // Re-validated at runtime: the map may come from an external keymap file.
  return action !== undefined && ACTION_IDS.includes(action) ? action : null;
}

/** A chord split into its modifiers and key. */
export interface ParsedChord {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
  key: string;
}

/** The canonical chord for `key` held with `modifiers`: `Ctrl+Alt+Shift+Meta+key`. */
export function chordOf(modifiers: Omit<ParsedChord, 'key'>, key: string): string {
  const parts: string[] = [];
  if (modifiers.ctrl) parts.push('Ctrl');
  if (modifiers.alt) parts.push('Alt');
  if (modifiers.shift) parts.push('Shift');
  if (modifiers.meta) parts.push('Meta');
  parts.push(key);
  return parts.join('+');
}

/**
 * Parse chord syntax: `Modifier+...+Key` with modifiers Ctrl, Alt, Shift, Meta in
 * that exact order. Zero modifiers are accepted here so callers can tell a chord
 * that lacks a modifier from one that is malformed; returns null when malformed.
 *
 * Key must be a single printable character or a multi-character key name
 * (ArrowRight, Enter, Space, Plus, ...). A lone uppercase letter is rejected: use
 * Shift as a modifier instead. A whitespace character is rejected: use Space.
 */
export function parseChord(chord: string): ParsedChord | null {
  if (typeof chord !== 'string') {
    return null;
  }

  const keyStart = chord.lastIndexOf('+') + 1;
  const key = chord.slice(keyStart);
  const modifierParts = keyStart === 0 ? [] : chord.slice(0, keyStart - 1).split('+');

  const modifierOrder = ['Ctrl', 'Alt', 'Shift', 'Meta'];
  let lastIndex = -1;

  for (const modifier of modifierParts) {
    const index = modifierOrder.indexOf(modifier);
    // An unknown modifier (-1) is never past the last one, so it is refused here too.
    if (index <= lastIndex) {
      return null;
    }
    lastIndex = index;
  }

  if (modifierOrder.includes(key)) {
    return null;
  }

  // Whitespace is spelled by name (Space) so a chord never hides an invisible key.
  if (/\s/.test(key)) {
    return null;
  }

  // A lone uppercase letter is rejected so that Shift is expressed as a
  // modifier rather than through the key itself.
  const isSinglePrintableChar = key.length === 1 && !/[A-Z]/.test(key);
  const isMultiCharSpecialKey = key.length > 1 && /^[A-Z]/.test(key);
  const isValidKey = isSinglePrintableChar || isMultiCharSpecialKey;
  if (!isValidKey) {
    return null;
  }

  return {
    ctrl: modifierParts.includes('Ctrl'),
    alt: modifierParts.includes('Alt'),
    shift: modifierParts.includes('Shift'),
    meta: modifierParts.includes('Meta'),
    key,
  };
}

/**
 * Validate chord syntax and return the canonical form if valid, null otherwise.
 * At least one modifier is required.
 */
export function validateChord(chord: string): string | null {
  const parsed = parseChord(chord);
  if (parsed === null || !(parsed.ctrl || parsed.alt || parsed.shift || parsed.meta)) {
    return null;
  }
  return chord;
}
