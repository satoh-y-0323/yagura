/**
 * Where the web view's own right-click menu may open. Outside a field the
 * user can type into, that menu offers nothing for this app and its reload
 * item throws away every open view, so only an editable field keeps its
 * operating system's cut / copy / paste menu.
 */

/** The facts about the element under the pointer the decision reads. */
export interface ContextMenuTarget {
  /** The element's tag name, in either case. */
  tagName: string;
  /** An input's `type`; ignored for every other element. */
  type?: string;
  readOnly?: boolean;
  disabled?: boolean;
  /** Whether the element, or one it sits in, is content-editable. */
  isContentEditable?: boolean;
}

/** Input types a user types text into; every other one (checkbox, range, …) is not a text field. */
const TEXT_INPUT_TYPES = new Set(['text', 'search', 'url', 'tel', 'email', 'password', 'number']);

/** Whether a right-click on `target` keeps the web view's own menu. */
export function keepsNativeContextMenu(target: ContextMenuTarget | null): boolean {
  if (target === null) {
    return false;
  }
  if (target.isContentEditable === true) {
    return true;
  }
  if (target.readOnly === true || target.disabled === true) {
    return false;
  }
  const tag = target.tagName.toLowerCase();
  if (tag === 'textarea') {
    return true;
  }
  return tag === 'input' && TEXT_INPUT_TYPES.has((target.type ?? 'text').toLowerCase());
}

/**
 * Stops the web view's own menu everywhere but an editable field. A handler
 * that already took the right-click (a window's own menu) is left alone.
 */
export function suppressNativeContextMenu(event: MouseEvent): void {
  if (event.defaultPrevented) {
    return;
  }
  const target = event.target instanceof HTMLElement ? event.target : null;
  if (!keepsNativeContextMenu(target)) {
    event.preventDefault();
  }
}
