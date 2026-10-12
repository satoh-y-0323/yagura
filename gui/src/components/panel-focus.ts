/**
 * Escape handling for the side panels. A panel closes only on an unmodified,
 * finished, unhandled Escape, so a CLI in a window and the chat mention list
 * keep their own Escape.
 */

import { isImeKey, type ImeKeyLike } from '../keymap/key-chord';

/** The fields of a keydown the panel Escape rule reads. */
export interface PanelKeyLike extends ImeKeyLike {
  key: string;
  altKey: boolean;
  shiftKey: boolean;
  defaultPrevented: boolean;
}

/** Whether `event` is an Escape that should close a panel. */
export function isPanelCloseKey(event: PanelKeyLike): boolean {
  return (
    event.key === 'Escape' &&
    !event.ctrlKey &&
    !event.altKey &&
    !event.shiftKey &&
    !event.metaKey &&
    !isImeKey(event) &&
    !event.defaultPrevented
  );
}

/** What {@link bodyEscapePanel} reads besides the key. */
export interface BodyEscapeContext<P> {
  /** Whether focus sits on the body, where no panel receives the key. */
  focusOnBody: boolean;
  isModalOpen: boolean;
  /** The panel that last held focus, cleared once focus moved elsewhere. */
  lastFocusedPanel: P | null;
  isOpen: (panel: P) => boolean;
}

/**
 * The panel a panel-closing Escape closes while focus is on the body, `null`
 * for none. Focus falls to the body when a focused button inside a panel
 * becomes disabled, so the panel no longer receives its Escape.
 */
export function bodyEscapePanel<P>(event: PanelKeyLike, context: BodyEscapeContext<P>): P | null {
  if (!isPanelCloseKey(event) || !context.focusOnBody || context.isModalOpen) {
    return null;
  }
  const panel = context.lastFocusedPanel;
  return panel !== null && context.isOpen(panel) ? panel : null;
}

/**
 * Svelte action: calls `onClose` for a panel-closing Escape that bubbles up to
 * `node`. It listens in the bubble phase so a handler nearer the key (the chat
 * mention list) can claim the Escape first.
 */
export function closeOnEscape(
  node: EventTarget,
  onClose: () => void
): { update(next: () => void): void; destroy(): void } {
  let close = onClose;
  const handleKeydown = (event: Event): void => {
    if (isPanelCloseKey(event as KeyboardEvent)) {
      close();
    }
  };
  node.addEventListener('keydown', handleKeydown);
  return {
    update(next: () => void): void {
      close = next;
    },
    destroy(): void {
      node.removeEventListener('keydown', handleKeydown);
    },
  };
}
