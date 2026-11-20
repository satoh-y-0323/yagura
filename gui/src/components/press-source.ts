/**
 * Whether a button's `click` came from the keyboard (Enter or Space) rather
 * than a pointer. A click from a key carries a click count (`detail`) of 0; a
 * pointer press counts at least 1.
 * @param event the `click` the button received
 */
export function pressedByKeyboard(event: Pick<MouseEvent, 'detail'>): boolean {
  return event.detail === 0;
}
