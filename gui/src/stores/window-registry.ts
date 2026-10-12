/**
 * The single owner of "which windows are open, which one is active, and which
 * events go to which window".
 *
 * The registry is a store rather than module state so a test (and a future
 * second application root) can hold an independent instance, and so the view
 * reads the window list and every window's state through
 * one subscription.
 */

import { writable, type Readable } from 'svelte/store';

import { bringToFront, MIN_WINDOW_SIZE, type Rect, type Size } from '../layout/float-layout';
import { tileRects } from '../layout/tile-layout';
import { reorderTabs } from '../layout/tab-order';
import type { DetectedStatusView } from '../ipc/session-boundary';
import type { WindowEventHandlers, WindowInfo } from '../ipc/session-client';

/** Screen position of a window pane. */
type Position = { x: number; y: number };

/** Pixel dimensions of a window pane. */
type WindowSize = { width: number; height: number };

/**
 * Presentation attributes of one window.
 */
export interface WindowDisplay {
  /** Position of the pane, `null` while the layout places it. */
  position: Position | null;
  /** Size of the pane, `null` while the layout sizes it. */
  size: WindowSize | null;
  /** Stacking order among the open windows; higher is nearer the front. */
  zOrder: number;
  /** Whether the window is collapsed to the taskbar/dock. */
  isMinimized: boolean;
  /** Whether the window currently fills the workspace. */
  isMaximized: boolean;
  /** Position and size to restore when the window is un-maximized, `null` when not maximized. */
  restoreBounds: Rect | null;
}

/** One open window, as the view needs it. */
export interface WindowEntry {
  /** Opaque window id assigned by the daemon. */
  windowId: string;
  /** Display name of the window. */
  name: string;
  /** Vendor identifier, `null` when the window is not attributed to one. */
  vendor: string | null;
  /** Absolute working directory of the child process. */
  cwd: string;
  /** Last known status of the child process, with the reason it is `'unknown'`. */
  detected: DetectedStatusView;
  /**
   * The generation `detected` was observed at, `null` while no observation of
   * this window has named one.
   */
  statusRev: number | null;
  /** Exit code once the child has exited, `null` otherwise. */
  exitCode: number | null;
  /** The room the window belongs to, `null` when the daemon reported none. */
  room: { roomId: string; name: string } | null;
  /** Detection-load warning describing a definition that failed to load, `null` when there is none. */
  detectionWarning: string | null;
  /**
   * Whether the window's child is currently connected to the daemon over
   * MCP, `null` while the daemon has reported nothing about it.
   */
  mcpConnected: boolean | null;
  /**
   * Whether the window's CLI is expected to report its turns but none has
   * arrived, `null` while the daemon has reported nothing about it.
   */
  statusReportsMissing: boolean | null;
  /** Presentation attributes; see {@link WindowDisplay}. */
  display: WindowDisplay;
}

/** Observable state of the whole window set. */
export interface WindowRegistryState {
  /** The open windows, in the order they were opened. */
  windows: WindowEntry[];
  /**
   * The window ids in the order their tabs are shown, which the user can
   * rearrange independently of the order the windows were opened in. It holds
   * exactly the ids in `windows`, without duplicates; it lives here so
   * a window and its tab appear and disappear in the same update.
   */
  tabOrder: string[];
}

/** Store of [`WindowRegistryState`] with the operations the window set supports. */
export interface WindowRegistry extends Readable<WindowRegistryState> {
  /**
   * Adds a window. Passing a
   * window that is already open refreshes its display name, its room and its
   * detection warning instead of adding a second entry, and moves its pair
   * only when the snapshot is at least as new as the one the entry holds.
   */
  openWindow: (info: WindowInfo) => void;
  /** Changes a window's display name; unknown window ids are ignored. */
  renameWindow: (windowId: string, name: string) => void;
  /** Removes a window and its handlers. */
  closeWindow: (windowId: string) => void;
  /**
   * Records a status change; `exited` removes the window. The pair replaces
   * the entry's whole `detected`, and is discarded when `statusRev` names a
   * generation older than the one the entry already holds.
   */
  setStatus: (windowId: string, detected: DetectedStatusView, statusRev: number | null) => void;
  /** Routes this window's events to `handlers`. */
  registerWindowHandlers: (windowId: string, handlers: WindowEventHandlers) => void;
  /** Returns the handlers of a window, or `undefined` when it is not open. */
  resolveWindowHandlers: (windowId: string) => WindowEventHandlers | undefined;
  /** Moves an open window's pane; unknown window ids are ignored. */
  setWindowPosition: (windowId: string, position: Position) => void;
  /** Resizes an open window's pane; unknown window ids are ignored. */
  setWindowSize: (windowId: string, size: WindowSize) => void;
  /** Sets whether an open window is minimized; unknown window ids are ignored. */
  setWindowMinimized: (windowId: string, minimized: boolean) => void;
  /**
   * Sets whether each of `windowIds` is minimized, writing the whole set in
   * one store update so subscribers are notified once. Unknown window ids are
   * ignored; an empty list leaves the state unchanged.
   */
  setWindowsMinimized: (windowIds: readonly string[], minimized: boolean) => void;
  /**
   * Sets whether an open window is maximized. At most one window is
   * maximized at a time: maximizing one restores any other maximized
   * window. Unknown window ids are ignored.
   */
  setWindowMaximized: (windowId: string, maximized: boolean) => void;
  /**
   * Raises an open window above the others, delegating the stacking-order
   * computation to {@link bringToFront}. Restores any other maximized
   * window, since only the front window may stay maximized. Unknown window
   * ids are ignored.
   */
  bringWindowToFront: (windowId: string) => void;
  /**
   * Moves a window's tab to `targetIndex` in the tab order, clamped to the
   * ends. The window list itself keeps the order the windows were opened in.
   * Unknown window ids are ignored.
   */
  moveTab: (windowId: string, targetIndex: number) => void;
  /**
   * Tiles every open window: {@link arrangeWindowsOf} applied to the whole tab
   * order, so no window falls outside the set and none is left minimized.
   * Re-tiling only ever happens on such an explicit call; nothing re-tiles the
   * windows implicitly.
   */
  arrangeWindows: (area: Size) => void;
  /**
   * Tiles `windowIds` into an equal-size grid filling `area`, in tab order
   * rather than in the order given, and minimizes every open window outside
   * that set in the same update, so the workspace shows the set alone. The
   * targets are placed un-minimized and un-maximized, a minimized one included.
   * Subscribers are notified once. The state is left unchanged when `windowIds`
   * is empty, when it holds an id that is not open, or when the grid does not
   * yield exactly one cell per target; no store update happens at all when
   * `area` is smaller than {@link MIN_WINDOW_SIZE} in either dimension.
   */
  arrangeWindowsOf: (area: Size, windowIds: readonly string[]) => void;
}

/** Creates an empty window registry. */
export function createWindowRegistry(): WindowRegistry {
  const { subscribe, update } = writable<WindowRegistryState>({
    windows: [],
    tabOrder: [],
  });
  // Handlers live outside the store because they are routing targets, not
  // rendered state: putting them in the store would make every output event
  // look like a state change to subscribers.
  const handlersByWindow = new Map<string, WindowEventHandlers>();

  const removeWindow = (windowId: string): void => {
    handlersByWindow.delete(windowId);
    update((state) => withoutWindow(state, windowId));
  };

  const updateDisplay = (
    windowId: string,
    change: (display: WindowDisplay) => WindowDisplay
  ): void => {
    update((state) => withDisplay(state, windowId, change));
  };

  const setWindowsMinimized = (windowIds: readonly string[], minimized: boolean): void => {
    update((state) => withMinimized(state, windowIds, minimized));
  };

  // Both tiling entry points share one implementation; they differ only in
  // which window ids the target set is read from.
  const tileWindows = (
    area: Size,
    targetsOf: (state: WindowRegistryState) => readonly string[]
  ): void => {
    if (area.width < MIN_WINDOW_SIZE.width || area.height < MIN_WINDOW_SIZE.height) {
      return;
    }
    update((state) => {
      const windows = withTiledDisplay(state, area, targetsOf(state));
      if (windows === null) {
        return state;
      }
      return { ...state, windows };
    });
  };

  return {
    subscribe,
    openWindow: (info) => {
      update((state) => {
        if (state.windows.some((entry) => entry.windowId === info.window_id)) {
          // A snapshot of an already-open window is a refresh, not a second
          // window. The name and the room are taken from it because they are
          // the two fields the daemon can change without the view being told
          // (a rename issued elsewhere; a room name that resolves differently
          // once another room appears), while status arrives through its own
          // event.
          return withSnapshot(state, info);
        }
        return {
          ...state,
          windows: [...state.windows, toEntry(info, state.windows.length)],
          tabOrder: [...state.tabOrder, info.window_id],
        };
      });
    },
    renameWindow: (windowId, name) => {
      update((state) => withName(state, windowId, name));
    },
    closeWindow: removeWindow,
    setStatus: (windowId, detected, statusRev) => {
      if (detected.status === 'exited') {
        removeWindow(windowId);
        return;
      }
      update((state) => ({
        ...state,
        windows: state.windows.map((entry) =>
          entry.windowId === windowId ? withObservedPair(entry, detected, statusRev) : entry
        ),
      }));
    },
    registerWindowHandlers: (windowId, handlers) => {
      handlersByWindow.set(windowId, handlers);
    },
    resolveWindowHandlers: (windowId) => handlersByWindow.get(windowId),
    setWindowPosition: (windowId, position) => {
      updateDisplay(windowId, (display) => ({ ...display, position }));
    },
    setWindowSize: (windowId, size) => {
      updateDisplay(windowId, (display) => ({ ...display, size }));
    },
    setWindowMinimized: (windowId, minimized) => {
      setWindowsMinimized([windowId], minimized);
    },
    setWindowsMinimized,
    setWindowMaximized: (windowId, maximized) => {
      update((state) => {
        if (!state.windows.some((entry) => entry.windowId === windowId)) {
          return state;
        }
        return {
          ...state,
          windows: state.windows.map((entry) => {
            if (entry.windowId === windowId) {
              return { ...entry, display: applyMaximized(entry.display, maximized) };
            }
            // At most one window is maximized: maximizing this one restores
            // any other window that was maximized before it.
            if (maximized && entry.display.isMaximized) {
              return { ...entry, display: restoreFromMaximized(entry.display) };
            }
            return entry;
          }),
        };
      });
    },
    bringWindowToFront: (windowId) => {
      update((state) => {
        if (!state.windows.some((entry) => entry.windowId === windowId)) {
          return state;
        }
        const zOrders = new Map(
          state.windows.map((entry) => [entry.windowId, entry.display.zOrder])
        );
        const raised = bringToFront(zOrders, windowId);
        return {
          ...state,
          windows: state.windows.map((entry) => {
            const zOrder = raised.get(entry.windowId) ?? entry.display.zOrder;
            const display = { ...entry.display, zOrder };
            // Only the front window may stay maximized.
            if (entry.windowId !== windowId && display.isMaximized) {
              return { ...entry, display: restoreFromMaximized(display) };
            }
            return { ...entry, display };
          }),
        };
      });
    },
    moveTab: (windowId, targetIndex) => {
      update((state) => ({
        ...state,
        tabOrder: reorderTabs(state.tabOrder, windowId, targetIndex),
      }));
    },
    arrangeWindows: (area) => {
      tileWindows(area, (state) => state.tabOrder);
    },
    arrangeWindowsOf: (area, windowIds) => {
      tileWindows(area, () => windowIds);
    },
  };
}

/** The wire's room, in the view's naming; `null` when the snapshot carries none. */
function roomOf(info: WindowInfo): { roomId: string; name: string } | null {
  return info.room === undefined ? null : { roomId: info.room.room_id, name: info.room.name };
}

function toEntry(info: WindowInfo, zOrder: number): WindowEntry {
  return {
    windowId: info.window_id,
    name: info.name,
    vendor: info.vendor,
    cwd: info.cwd,
    detected: info.detected,
    statusRev: info.status_rev ?? null,
    exitCode: info.exit_code,
    room: roomOf(info),
    detectionWarning: info.detection_warning ?? null,
    mcpConnected: info.mcp_connected ?? null,
    statusReportsMissing: info.status_reports_missing ?? null,
    display: {
      position: null,
      size: null,
      zOrder,
      isMinimized: false,
      isMaximized: false,
      restoreBounds: null,
    },
  };
}

function withName(state: WindowRegistryState, windowId: string, name: string): WindowRegistryState {
  return {
    ...state,
    windows: state.windows.map((entry) =>
      entry.windowId === windowId ? { ...entry, name } : entry
    ),
  };
}

/**
 * Applies an observation of a window's status pair, unless it was taken at a
 * generation older than the one the entry holds.
 *
 * An observation naming no generation is applied without moving the recorded
 * one: the recorded generation is a high-water mark, so a stale numbered
 * observation arriving afterwards is still refused.
 */
function withObservedPair(
  entry: WindowEntry,
  detected: DetectedStatusView,
  statusRev: number | null
): WindowEntry {
  if (statusRev !== null && entry.statusRev !== null && statusRev < entry.statusRev) {
    return entry;
  }
  return { ...entry, detected, statusRev: statusRev ?? entry.statusRev };
}

/**
 * Refreshes the fields a snapshot of an already-open window may carry anew.
 *
 * The display name, the room, the detection warning and the MCP connection
 * land whatever the generation says: they are what the list alone reports,
 * and showing a name the daemon no longer uses would be worse than showing
 * an older one. The status pair is the only field a stale snapshot may not
 * move.
 */
function withSnapshot(state: WindowRegistryState, info: WindowInfo): WindowRegistryState {
  return {
    ...state,
    windows: state.windows.map((entry) =>
      entry.windowId === info.window_id
        ? {
            ...withObservedPair(entry, info.detected, info.status_rev ?? null),
            name: info.name,
            room: roomOf(info),
            detectionWarning: info.detection_warning ?? null,
            mcpConnected: info.mcp_connected ?? null,
            statusReportsMissing: info.status_reports_missing ?? null,
          }
        : entry
    ),
  };
}

/** Writes `isMinimized` on every listed window at once; unknown ids match nothing. */
function withMinimized(
  state: WindowRegistryState,
  windowIds: readonly string[],
  minimized: boolean
): WindowRegistryState {
  const targets = new Set(windowIds);
  return {
    ...state,
    windows: state.windows.map((entry) =>
      targets.has(entry.windowId)
        ? { ...entry, display: { ...entry.display, isMinimized: minimized } }
        : entry
    ),
  };
}

function withDisplay(
  state: WindowRegistryState,
  windowId: string,
  change: (display: WindowDisplay) => WindowDisplay
): WindowRegistryState {
  return {
    ...state,
    windows: state.windows.map((entry) =>
      entry.windowId === windowId ? { ...entry, display: change(entry.display) } : entry
    ),
  };
}

/** Snapshots the current position and size, or `null` when either is unset. */
function captureRect(display: WindowDisplay): Rect | null {
  if (display.position === null || display.size === null) {
    return null;
  }
  return { position: display.position, size: display.size };
}

function applyMaximized(display: WindowDisplay, maximized: boolean): WindowDisplay {
  if (!maximized) {
    return restoreFromMaximized(display);
  }
  if (display.isMaximized) {
    // Already maximized: keep the rectangle captured the first time so a
    // repeated maximize does not overwrite it with the maximized bounds.
    return display;
  }
  return { ...display, isMaximized: true, restoreBounds: captureRect(display) };
}

/** Restores the pre-maximize bounds and clears the maximized state. */
function restoreFromMaximized(display: WindowDisplay): WindowDisplay {
  return {
    ...display,
    isMaximized: false,
    restoreBounds: null,
    position: display.restoreBounds?.position ?? display.position,
    size: display.restoreBounds?.size ?? display.size,
  };
}

/**
 * Tiles `windowIds` (in `state.tabOrder` order) into `area` while minimizing
 * every window outside that set; `null` leaves the caller's state as it is.
 */
function withTiledDisplay(
  state: WindowRegistryState,
  area: Size,
  windowIds: readonly string[]
): WindowEntry[] | null {
  const targets = new Set(windowIds);
  if (targets.size === 0) {
    return null;
  }
  const ordered = state.tabOrder.filter((windowId) => targets.has(windowId));
  const rects = tileRects(area, ordered.length);
  if (rects.length !== ordered.length) {
    return null;
  }
  const rectByWindowId = new Map<string, Rect>(
    // The length check above pins rects.length to ordered.length, so every
    // index below is in bounds (`!` is banned by lint).
    ordered.map((windowId, index) => [windowId, rects[index] as Rect])
  );
  if (!windowIds.every((windowId) => rectByWindowId.has(windowId))) {
    return null;
  }

  // Spreading each display keeps zOrder: tiling changes geometry, never stacking order.
  return state.windows.map((entry) => {
    const rect = rectByWindowId.get(entry.windowId);
    if (rect === undefined) {
      return {
        ...entry,
        display: {
          ...entry.display,
          isMinimized: true,
          isMaximized: false,
          restoreBounds: null,
        },
      };
    }
    // The tiled rect is final geometry, not a value to restore into, so not restoreFromMaximized.
    return {
      ...entry,
      display: {
        ...entry.display,
        position: rect.position,
        size: rect.size,
        isMinimized: false,
        isMaximized: false,
        restoreBounds: null,
      },
    };
  });
}

/** Drops a window together with its tab. */
function withoutWindow(state: WindowRegistryState, windowId: string): WindowRegistryState {
  const windows = state.windows.filter((entry) => entry.windowId !== windowId);
  // Filtered here rather than by a separate mutator so the tab disappears in
  // the same update as the window it stands for.
  const tabOrder = state.tabOrder.filter((id) => id !== windowId);
  return { ...state, windows, tabOrder };
}
