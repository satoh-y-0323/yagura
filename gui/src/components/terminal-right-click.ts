import type { KeyboardPlatform } from '../keymap/keyboard-platform';

/** What a right-click on a window does. */
export type RightClickAction = 'pass-to-app' | 'copy' | 'paste' | 'menu';

/** The facts a right-click is decided on. */
export interface RightClickContext {
  platform: KeyboardPlatform;
  /** The program in the window has turned on mouse reporting. */
  appCapturesMouse: boolean;
  shiftKey: boolean;
  hasSelection: boolean;
}

/**
 * Follows each platform's terminal convention: Windows copies a selection or
 * pastes, macOS and Linux open a menu. A program that captures the mouse
 * receives the click as Windows Terminal hands it over, and Shift takes it back;
 * pasting as well would paste twice into a program that pastes on right-click.
 */
export function decideRightClick(context: RightClickContext): RightClickAction {
  if (context.appCapturesMouse && !context.shiftKey) {
    return 'pass-to-app';
  }
  if (context.platform !== 'windows') {
    return 'menu';
  }
  return context.hasSelection ? 'copy' : 'paste';
}
