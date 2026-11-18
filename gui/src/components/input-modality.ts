/**
 * Whether the user's last input was the keyboard or a pointer. A tooltip asks
 * this as well as `:focus-visible`, because WebKit matches `:focus-visible` for
 * every focus a script moves (a dialog's initial focus, focus returned on
 * close), even right after a pointer press; Chromium does not.
 */
export interface InputModality {
  isKeyboard: () => boolean;
  dispose: () => void;
}

// A modifier pressed on its own (Ctrl before a click, say) is not keyboard use.
const MODIFIER_KEYS = new Set(['Shift', 'Control', 'Alt', 'Meta', 'AltGraph', 'CapsLock']);

// Capture, so that a handler that stops the event does not hide it.
const CAPTURE = { capture: true };

/** Follows the inputs `target` sees from now on; a pointer until the first key. */
export function createInputModality(
  target: Pick<EventTarget, 'addEventListener' | 'removeEventListener'>
): InputModality {
  let isKeyboard = false;

  const handleKeyDown = (event: Event): void => {
    if (!MODIFIER_KEYS.has((event as KeyboardEvent).key)) {
      isKeyboard = true;
    }
  };
  const handlePointerDown = (): void => {
    isKeyboard = false;
  };

  target.addEventListener('keydown', handleKeyDown, CAPTURE);
  target.addEventListener('pointerdown', handlePointerDown, CAPTURE);

  return {
    isKeyboard: () => isKeyboard,
    dispose: () => {
      target.removeEventListener('keydown', handleKeyDown, CAPTURE);
      target.removeEventListener('pointerdown', handlePointerDown, CAPTURE);
    },
  };
}

let shared: InputModality | undefined;

/** The application's one tracker, started on first use. */
export function sharedInputModality(): InputModality {
  shared ??= createInputModality(window);
  return shared;
}

/**
 * Whether `node` holds focus the way a keyboard user gets it: `:focus-visible`
 * and a last input that was a key. The single place the application reads
 * `:focus-visible` from script, so every focus-driven display agrees.
 */
export function isKeyboardFocus(node: Element): boolean {
  return node.matches(':focus-visible') && sharedInputModality().isKeyboard();
}
