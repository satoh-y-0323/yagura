/**
 * What the view sends a window for the DOM events that are not pointer
 * buttons: a paste, the wheel, and a keydown. `null` is nothing to send.
 */
import type { WindowInput } from '../ipc/session-client';
import type { KeyboardPlatform } from '../keymap/keyboard-platform';
import { decideKeyDown, type KeyDownFacts, type Modifiers } from './key-input';
import { wheelLines, type CellPosition } from './pointer-input';

/** Typed or committed text; none sends nothing. */
export function textInput(text: string | null): WindowInput | null {
  return text === null ? null : { type: 'text', text };
}

/** A paste of `text`; an empty paste sends nothing. */
export function pasteInput(text: string): WindowInput | null {
  return text === '' ? null : { type: 'paste', text };
}

/** A wheel turn; a turn with no direction sends nothing, and the cell is not looked up. */
export function wheelInput(
  event: { deltaY: number },
  cellOf: () => CellPosition,
  mods: Modifiers
): WindowInput | null {
  const delta = wheelLines(event);
  return delta === 0 ? null : { type: 'wheel', delta, cell: cellOf(), mods };
}

/** What a keydown does to the page and to the window. */
export interface KeyEffect {
  /** The browser's own handling of the key is suppressed. */
  preventsDefault: boolean;
  clipboard: 'copy' | 'paste' | null;
  input: WindowInput | null;
}

/** A key the browser keeps (typing goes through the input event) has no effect here. */
export function keyDownEffect(event: KeyDownFacts, platform: KeyboardPlatform): KeyEffect {
  const decision = decideKeyDown(event, platform);
  switch (decision.kind) {
    case 'browser':
      return { preventsDefault: false, clipboard: null, input: null };
    case 'clipboard':
      return { preventsDefault: true, clipboard: decision.action, input: null };
    case 'key':
      return { preventsDefault: true, clipboard: null, input: { type: 'key', ...decision.input } };
  }
}
