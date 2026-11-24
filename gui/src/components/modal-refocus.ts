/** The only element a refocus may move focus to, mirroring the dialog focusing steps. */
const AUTOFOCUS_SELECTOR = '[autofocus]:not([disabled])';

/** The slice of a dialog element the refocus needs. */
export interface AutofocusRoot {
  querySelector: (selector: string) => { focus: () => void } | null;
}

/**
 * Whether an open modal whose subject changed to a new one must pull focus
 * back to its safe control. Closing or clearing the subject never does.
 */
export function shouldRefocus(
  previousKey: string | null,
  nextKey: string | null,
  isOpen: boolean
): boolean {
  return isOpen && nextKey !== null && nextKey !== previousKey;
}

/** Focuses the root's autofocus element; moves focus nowhere else when there is none. */
export function focusAutofocusDelegate(root: AutofocusRoot): void {
  root.querySelector(AUTOFOCUS_SELECTOR)?.focus();
}
