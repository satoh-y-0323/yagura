/**
 * Follows whether the app as a whole is off screen: the main window minimized
 * (read when Tauri reports a size change) or the document hidden.
 */

import { getCurrentWindow } from '@tauri-apps/api/window';
import { readable, type Readable } from 'svelte/store';

import { appIsHidden } from './effective-visibility';

/** `true` while the app is off screen. Tracking runs while the store has a subscriber. */
export const appHidden: Readable<boolean> = readable(false, (set) => {
  const mainWindow = getCurrentWindow();
  let isMinimized = false;
  let isActive = true;

  const publish = (): void => {
    set(appIsHidden({ isMinimized, visibilityState: document.visibilityState }));
  };
  const readMinimized = (): void => {
    mainWindow
      .isMinimized()
      .then((minimized) => {
        if (isActive) {
          isMinimized = minimized;
          publish();
        }
      })
      .catch((error: unknown) => {
        console.warn('failed to read whether the main window is minimized:', error);
      });
  };

  document.addEventListener('visibilitychange', publish);
  const unlistenResized = mainWindow.onResized(readMinimized);
  readMinimized();
  publish();

  return () => {
    isActive = false;
    document.removeEventListener('visibilitychange', publish);
    unlistenResized
      .then((unlisten) => {
        unlisten();
      })
      .catch((error: unknown) => {
        console.warn('failed to stop listening to the main window size:', error);
      });
  };
});
