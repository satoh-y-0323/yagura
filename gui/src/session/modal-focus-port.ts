/**
 * The port a modal dialog reports its opening and closing through, so the
 * application can decide where focus goes once it has closed
 * (`modalReturnTarget` in `focus-moves.ts`). Kept apart from `focus-moves.ts`,
 * which stays free of Svelte.
 */
import { getContext, setContext } from 'svelte';

/** Identifies one opening of a modal dialog. */
export type ModalFocusToken = number;

/** What the application provides to every modal dialog below it. */
export interface ModalFocusPort {
  /**
   * Called right before the dialog opens, while focus is still where the user was.
   *
   * @param subject the window the dialog is about, or null
   */
  opened(subject: string | null): ModalFocusToken;
  /** Called once per opening, whether the dialog was closed or torn down while open. */
  closed(token: ModalFocusToken): void;
}

const MODAL_FOCUS_PORT = Symbol('modal-focus-port');

/** Provides `port` to the modal dialogs of the calling component's subtree. */
export function provideModalFocusPort(port: ModalFocusPort): void {
  setContext(MODAL_FOCUS_PORT, port);
}

/** The port provided above the calling component, if any. */
export function modalFocusPort(): ModalFocusPort | undefined {
  return getContext<ModalFocusPort | undefined>(MODAL_FOCUS_PORT);
}
