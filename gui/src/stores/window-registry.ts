/**
 * The single owner of "which windows are open, which one is active, and which
 * events go to which window".
 *
 * The registry is a store rather than module state so a test (and a future
 * second application root) can hold an independent instance, and so the view
 * reads the window list and every window's state through
 * one subscription.
 */

import { get, writable, type Readable } from 'svelte/store';

import { bringToFront, MIN_WINDOW_SIZE, type Rect, type Size } from '../layout/float-layout';
import { tileRects } from '../layout/tile-layout';
import {
  formatPathForDisplay,
  sanitizeForDisplay,
  type DisplayLine,
} from '../display/untrusted-text';
import { reorderTabs } from '../sidebar/row-order';
import {
  type DetectedStatusView,
  type ReportState,
  type StatusBoot,
  type StatusPayload,
  type TaskReportMark,
} from '../ipc/session-boundary';
import type { WindowEventHandlers, WindowInfo } from '../ipc/session-client';
import type { WindowMark } from '../session/approval-types';

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
  /** `windowId` prepared for display. */
  displayWindowId: DisplayLine;
  /** The mark the daemon drew from `windowId`; it never changes for one window. */
  mark: WindowMark;
  /** Name of the window as the daemon sent it; for operations, never drawn. */
  name: string;
  /** `name` prepared for display; the only form a view draws. */
  displayName: DisplayLine;
  /** Vendor identifier, `null` when the window is not attributed to one. */
  vendor: string | null;
  /** `vendor` prepared for display, `null` when `vendor` is. */
  displayVendor: DisplayLine | null;
  /** Absolute working directory of the child process, as sent; never drawn. */
  cwd: string;
  /** `cwd` prepared for display. */
  displayCwd: DisplayLine;
  /** Last known status of the child process, with the reason it is `'unknown'`. */
  detected: DetectedStatusView;
  /**
   * The generation `detected` was observed at, `null` while no observation of
   * this window has named one.
   */
  statusRev: number | null;
  /** The daemon boot `statusRev` was counted in, `null` when none was named. */
  statusBoot: StatusBoot | null;
  /**
   * The room the window belongs to, `null` when the daemon reported none.
   * `displayName` is the room's name prepared for display.
   */
  room: { roomId: string; displayName: DisplayLine } | null;
  /**
   * The detection-load warning describing a definition that failed to load,
   * prepared for display; `null` when there is none.
   */
  displayDetectionWarning: DisplayLine | null;
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
  /**
   * How many messages wait in the window's inbox, present only while at least
   * one does; see {@link queuedCountOf}.
   */
  queuedCount?: number;
  /**
   * The window's report mark, present only while the daemon has reported one
   * this boot; merged on its own key by {@link withTaskReport}.
   */
  taskReport?: TaskReportMark;
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
  /**
   * The windows this update retired, each with the code it ended with
   * (`null` when the code could not be determined). Empty in every other
   * update, so a subscriber reads a code once, in the update that removes
   * its window.
   */
  retired: readonly RetiredWindow[];
}

/** One window {@link WindowRegistry.retireWindow} removed, with the code it ended with. */
export interface RetiredWindow {
  windowId: string;
  exitCode: number | null;
}

/** The code an ended window is retired with; see {@link retirementFromStatus}. */
export interface Retirement {
  exitCode: number | null;
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
   * Removes a window that ended, with its tab and handlers, in one update
   * whose `retired` names it and `exitCode`. A window that is not listed is
   * left alone and no update is published.
   */
  retireWindow: (windowId: string, exitCode: number | null) => void;
  /**
   * Records a status change; `exited` included, which never removes the
   * window ({@link WindowRegistry.retireWindow} does). The pair replaces
   * the entry's whole `detected`, and is discarded when `(statusBoot,
   * statusRev)` names an observation older than the one the entry already
   * holds. `taskReport` is merged on its own key, whatever the pair's
   * generation says; omitting it means the observation carries no mark.
   */
  setStatus: (
    windowId: string,
    detected: DetectedStatusView,
    statusRev: number | null,
    statusBoot: StatusBoot | null,
    taskReport?: TaskReportMark
  ) => void;
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
  const store = writable<WindowRegistryState>({
    windows: [],
    tabOrder: [],
    retired: [],
  });
  const { subscribe } = store;
  // Every update but a retirement clears `retired`, so a code is never read
  // again from a later, unrelated update.
  const update = (change: (state: WindowRegistryState) => WindowRegistryState): void => {
    store.update((state) => {
      const next = change(state);
      return next.retired.length === 0 ? next : { ...next, retired: [] };
    });
  };
  // Handlers live outside the store because they are routing targets, not
  // rendered state: putting them in the store would make every output event
  // look like a state change to subscribers.
  const handlersByWindow = new Map<string, WindowEventHandlers>();

  const removeWindow = (windowId: string): void => {
    handlersByWindow.delete(windowId);
    update((state) => withoutWindow(state, windowId));
  };

  const retireWindow = (windowId: string, exitCode: number | null): void => {
    handlersByWindow.delete(windowId);
    if (!get(store).windows.some((entry) => entry.windowId === windowId)) {
      return;
    }
    store.update((state) => ({
      ...withoutWindow(state, windowId),
      retired: [{ windowId, exitCode }],
    }));
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
    retireWindow,
    setStatus: (windowId, detected, statusRev, statusBoot, taskReport) => {
      update((state) => ({
        ...state,
        windows: state.windows.map((entry) =>
          entry.windowId === windowId
            ? withObservedPair(
                withTaskReport(entry, statusBoot, taskReport),
                detected,
                statusRev,
                statusBoot
              )
            : entry
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

/**
 * The retirement an `exited` status push asks for, `null` when the status is
 * not `exited`. The push carries the window's exit code.
 */
export function retirementFromStatus(payload: StatusPayload): Retirement | null {
  if (payload.detected.status !== 'exited') {
    return null;
  }
  return { exitCode: payload.exit_code };
}

/**
 * The retirement a window-list entry asks for, `null` when the window has not
 * exited. A `-1` is the window's real exit code.
 */
export function retirementFromList(info: WindowInfo): Retirement | null {
  if (info.detected.status !== 'exited') {
    return null;
  }
  return { exitCode: info.exit_code };
}

/**
 * Hands one status push to `registry`: an `exited` push retires the window,
 * any other push records the status.
 * @param registry the windows the push is about
 * @param payload the push
 */
export function applyStatusPush(registry: WindowRegistry, payload: StatusPayload): void {
  const retirement = retirementFromStatus(payload);
  if (retirement !== null) {
    registry.retireWindow(payload.window_id, retirement.exitCode);
    return;
  }
  registry.setStatus(
    payload.window_id,
    payload.detected,
    payload.status_rev ?? null,
    payload.status_boot ?? null,
    payload.task_report
  );
}

/**
 * Hands one window-list entry to `registry`: an exited window is retired,
 * never opened, since the daemon lists it until it is evicted; any other is
 * opened or refreshed.
 * @param registry the windows the list is about
 * @param info the entry
 * @returns whether the window is still live
 */
export function applyListedWindow(registry: WindowRegistry, info: WindowInfo): boolean {
  const retirement = retirementFromList(info);
  if (retirement !== null) {
    registry.retireWindow(info.window_id, retirement.exitCode);
    return false;
  }
  registry.openWindow(info);
  return true;
}

/**
 * The count a window's waiting-messages mark shows, `null` when no mark is
 * shown: nothing waits, or the daemon predates the count.
 */
export function queuedCountOf(info: WindowInfo): number | null {
  const count = info.queued_count ?? 0;
  return count > 0 ? count : null;
}

/** The entry with its waiting-messages count replaced; `null` leaves the field off. */
function withQueuedCount(entry: WindowEntry, count: number | null): WindowEntry {
  if (count !== null) {
    return { ...entry, queuedCount: count };
  }
  if (entry.queuedCount === undefined) {
    return entry;
  }
  const cleared = { ...entry };
  delete cleared.queuedCount;
  return cleared;
}

/** `text` prepared for display, `null` staying `null`. */
function displayOrNull(text: string | null): DisplayLine | null {
  return text === null ? null : sanitizeForDisplay(text);
}

/**
 * The raw fields a snapshot can change and the display forms derived from
 * them. The one place untrusted text is prepared for the view.
 */
function snapshotFields(
  info: WindowInfo
): Pick<WindowEntry, 'name' | 'displayName' | 'room' | 'displayDetectionWarning'> {
  return {
    name: info.name,
    displayName: sanitizeForDisplay(info.name),
    room:
      info.room === undefined
        ? null
        : { roomId: info.room.room_id, displayName: formatPathForDisplay(info.room.name) },
    displayDetectionWarning: displayOrNull(info.detection_warning ?? null),
  };
}

function toEntry(info: WindowInfo, zOrder: number): WindowEntry {
  const entry: WindowEntry = {
    windowId: info.window_id,
    displayWindowId: sanitizeForDisplay(info.window_id),
    mark: info.mark,
    ...snapshotFields(info),
    vendor: info.vendor,
    displayVendor: displayOrNull(info.vendor),
    cwd: info.cwd,
    displayCwd: formatPathForDisplay(info.cwd),
    detected: info.detected,
    statusRev: info.status_rev ?? null,
    statusBoot: info.status_boot ?? null,
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
  return withTaskReport(
    withQueuedCount(entry, queuedCountOf(info)),
    entry.statusBoot,
    info.task_report
  );
}

function withName(state: WindowRegistryState, windowId: string, name: string): WindowRegistryState {
  return {
    ...state,
    windows: state.windows.map((entry) =>
      entry.windowId === windowId
        ? { ...entry, name, displayName: sanitizeForDisplay(name) }
        : entry
    ),
  };
}

/**
 * Applies an observation unless its key `(statusBoot, statusRev)` is older than the entry's.
 * A key it cannot be ordered against is taken whole: refusing could pin a gone daemon's pair.
 */
function withObservedPair(
  entry: WindowEntry,
  detected: DetectedStatusView,
  statusRev: number | null,
  statusBoot: StatusBoot | null
): WindowEntry {
  const held = entry.statusBoot;
  const olderRev = statusRev !== null && entry.statusRev !== null && statusRev < entry.statusRev;
  if (statusBoot === null && held === null) {
    return olderRev ? entry : { ...entry, detected, statusRev: statusRev ?? entry.statusRev };
  }
  if (statusBoot === null || held === null || statusBoot.origin !== held.origin) {
    return { ...entry, detected, statusRev, statusBoot };
  }
  const sameSeq = statusBoot.seq === held.seq;
  if (statusBoot.seq < held.seq || (sameSeq && olderRev)) {
    return entry;
  }
  return {
    ...entry,
    detected,
    statusRev: statusRev ?? (sameSeq ? entry.statusRev : null),
    statusBoot,
  };
}

/**
 * The order one report's state moves in within one `seq`. `unknown` ranks with
 * `no_reader`: neither is read as standing, nor known to be an instruction.
 */
const REPORT_STATE_RANK: Readonly<Record<ReportState, number>> = {
  standing: 0,
  no_reader: 1,
  unknown: 1,
  instruction: 2,
};

/**
 * Merges an observation's report mark, on a key apart from the status pair. Must
 * run before the pair moves `entry.statusBoot`: the boot it compares against is
 * the one the held mark was observed in.
 *
 * Another boot (another origin, a later boot, or either side naming none) is
 * taken whole, an absent mark clearing the one held. An earlier boot of the
 * same origin is ignored. Within one boot an absent mark keeps the held one,
 * and a mark replaces it only for a larger `seq`, or for the same `seq` when its
 * state ranks higher, so the order the routes arrive in never moves a mark back.
 */
function withTaskReport(
  entry: WindowEntry,
  statusBoot: StatusBoot | null,
  mark: TaskReportMark | undefined
): WindowEntry {
  const held = entry.statusBoot;
  const sameOrigin = statusBoot !== null && held !== null && statusBoot.origin === held.origin;
  if (sameOrigin && statusBoot.seq < held.seq) {
    return entry;
  }
  const sameBoot = sameOrigin && statusBoot.seq === held.seq;
  if (sameBoot) {
    const current = entry.taskReport;
    const replaces =
      mark !== undefined &&
      (current === undefined ||
        mark.seq > current.seq ||
        (mark.seq === current.seq &&
          REPORT_STATE_RANK[mark.state] > REPORT_STATE_RANK[current.state]));
    return replaces ? { ...entry, taskReport: mark } : entry;
  }
  if (mark !== undefined) {
    return { ...entry, taskReport: mark };
  }
  if (entry.taskReport === undefined) {
    return entry;
  }
  const cleared = { ...entry };
  delete cleared.taskReport;
  return cleared;
}

/**
 * Refreshes the fields a snapshot of an already-open window may carry anew.
 *
 * The display name, the room, the detection warning, the MCP connection and
 * the waiting-messages count land whatever the generation says: they are what the list alone reports,
 * and showing a name the daemon no longer uses would be worse than showing
 * an older one. The status pair is the only field a stale snapshot may not
 * move.
 */
function withSnapshot(state: WindowRegistryState, info: WindowInfo): WindowRegistryState {
  return {
    ...state,
    windows: state.windows.map((entry) =>
      entry.windowId === info.window_id
        ? withQueuedCount(
            {
              ...withObservedPair(
                withTaskReport(entry, info.status_boot ?? null, info.task_report),
                info.detected,
                info.status_rev ?? null,
                info.status_boot ?? null
              ),
              ...snapshotFields(info),
              mcpConnected: info.mcp_connected ?? null,
              statusReportsMissing: info.status_reports_missing ?? null,
            },
            queuedCountOf(info)
          )
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
