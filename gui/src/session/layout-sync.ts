/**
 * Reports live window-layout changes back to the daemon: watches the registry,
 * serializes each window's presentation layout as a {@link LayoutV1}, and calls
 * `send` for a window only once its layout differs from the last value observed
 * for it -- a change-only, debounced stream, never a full re-send of everything
 * on every store tick.
 */
import type { LayoutV1 } from './restore-model';
import type { WindowDisplay, WindowRegistry, WindowRegistryState } from '../stores/window-registry';

/** Trailing debounce window. */
const DEBOUNCE_MS = 300;

/**
 * Serializes one window's presentation layout. While maximized, `pos`/`size`
 * come from `restoreBounds` (the pre-maximize rectangle) rather than the
 * applied fullscreen rect, so a restore does not re-apply the fullscreen
 * geometry as if it were the floating one; a `null` `restoreBounds` (captured
 * before position/size were ever set) falls back to the current values.
 */
function buildLayoutV1(display: WindowDisplay, tabIndex: number): LayoutV1 {
  const bounds = display.isMaximized ? display.restoreBounds : null;
  return {
    v: 1,
    pos: bounds === null ? display.position : bounds.position,
    size: bounds === null ? display.size : bounds.size,
    z: display.zOrder,
    min: display.isMinimized,
    max: display.isMaximized,
    tab: tabIndex,
  };
}

/**
 * Subscribes to `registry` and calls `send(windowId, layoutJson)` whenever an
 * open window's layout changes, coalesced by a trailing debounce. A window's
 * first observation only records a baseline -- it is never sent -- so opening
 * the application does not report every already-open window's layout back to
 * the daemon.
 *
 * @returns a disposer that unsubscribes and discards any pending debounced
 * send (a send in flight when this fires is dropped, not flushed).
 */
export function startLayoutSync(
  registry: WindowRegistry,
  send: (windowId: string, layoutJson: string) => void
): () => void {
  const lastObserved = new Map<string, string>();
  const pendingTimers = new Map<string, ReturnType<typeof setTimeout>>();

  const clearPending = (windowId: string): void => {
    clearTimeout(pendingTimers.get(windowId));
    pendingTimers.delete(windowId);
  };

  const unsubscribe = registry.subscribe((state: WindowRegistryState) => {
    const openIds = new Set(state.windows.map((window) => window.windowId));
    for (const windowId of [...lastObserved.keys()]) {
      if (!openIds.has(windowId)) {
        lastObserved.delete(windowId);
        clearPending(windowId);
      }
    }

    for (const window of state.windows) {
      const tabIndex = state.tabOrder.indexOf(window.windowId);
      const serialized = JSON.stringify(buildLayoutV1(window.display, tabIndex));
      const previous = lastObserved.get(window.windowId);
      lastObserved.set(window.windowId, serialized);
      if (previous === undefined || previous === serialized) {
        continue;
      }
      clearPending(window.windowId);
      pendingTimers.set(
        window.windowId,
        setTimeout(() => {
          pendingTimers.delete(window.windowId);
          send(window.windowId, serialized);
        }, DEBOUNCE_MS)
      );
    }
  });

  return () => {
    unsubscribe();
    for (const timer of pendingTimers.values()) {
      clearTimeout(timer);
    }
  };
}
