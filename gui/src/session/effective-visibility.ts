/**
 * Whether a window is on screen, for the daemon's decision to build frames
 * for it. A pane being visible is not enough: the OS minimizing the whole app
 * is reported to neither the WebView nor the page, so it is read from the main
 * window, and a hidden document is the other way the app goes off screen.
 */

/** What the app's own state says about being on screen. */
export interface AppVisibilityInputs {
  /** The main window is minimized. */
  isMinimized: boolean;
  /** `document.visibilityState`. */
  visibilityState: DocumentVisibilityState;
}

/** The app as a whole is off screen. */
export function appIsHidden(inputs: AppVisibilityInputs): boolean {
  return inputs.isMinimized || inputs.visibilityState === 'hidden';
}

/** A window is on screen when its pane is visible and the app is not hidden. */
export function effectiveVisibility(paneVisible: boolean, appHidden: boolean): boolean {
  return paneVisible && !appHidden;
}
