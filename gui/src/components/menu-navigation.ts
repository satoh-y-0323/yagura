/**
 * Keyboard movement inside an open menu. Only enabled `menuitem*` entries can
 * take focus; separators, disabled items and plain text (a note) are skipped.
 */

export interface MenuEntry {
  /** The entry's ARIA role, or null for an element that has none (an explanatory sentence). */
  role: string | null;
  disabled: boolean;
}

export type MenuKeyResult =
  { type: 'focus'; index: number } | { type: 'close'; restoreFocus: boolean };

function canTakeFocus(entry: MenuEntry): boolean {
  return entry.role !== null && entry.role.startsWith('menuitem') && !entry.disabled;
}

/** The nearest focusable entry from `from` in `step` direction, wrapping at the ends. */
function nextFocusable(entries: readonly MenuEntry[], from: number, step: 1 | -1): number | null {
  const count = entries.length;
  for (let offset = 1; offset <= count; offset += 1) {
    const index = (((from + step * offset) % count) + count) % count;
    const entry = entries[index];
    if (entry !== undefined && canTakeFocus(entry)) {
      return index;
    }
  }
  return null;
}

function focusResult(index: number | null): MenuKeyResult | null {
  return index === null ? null : { type: 'focus', index };
}

/**
 * What a key does in a menu, or null when the menu does not handle it.
 * `current` is the focused entry's index, or -1 while focus is on the menu itself.
 */
export function handleMenuKey(
  key: string,
  entries: readonly MenuEntry[],
  current: number
): MenuKeyResult | null {
  switch (key) {
    case 'ArrowDown':
      return focusResult(nextFocusable(entries, current, 1));
    case 'ArrowUp':
      return focusResult(nextFocusable(entries, current === -1 ? 0 : current, -1));
    case 'Home':
      return focusResult(nextFocusable(entries, -1, 1));
    case 'End':
      return focusResult(nextFocusable(entries, 0, -1));
    case 'Escape':
      return { type: 'close', restoreFocus: true };
    case 'Tab':
      return { type: 'close', restoreFocus: false };
    default:
      return null;
  }
}
