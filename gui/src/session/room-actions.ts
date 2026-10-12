/**
 * The single execution path for the per-room operations: collapse toggling,
 * tiling a room's windows, and minimizing/restoring a room as a whole.
 *
 * The registry, the collapsed-room store and the two readers are injected at
 * construction time rather than imported, so this module has no module-level
 * singleton and stays usable with an independent registry instance per test
 * (or per application root).
 */

import type { Size } from '../layout/float-layout';

/** The subset of {@link WindowRegistry} this module needs. */
export interface RoomActionsRegistry {
  arrangeWindowsOf: (area: Size, windowIds: readonly string[]) => void;
  setWindowsMinimized: (windowIds: readonly string[], minimized: boolean) => void;
}

/** The subset of the collapsed-room store this module needs. */
export interface RoomActionsCollapsed {
  toggle: (roomId: string) => void;
}

/** Dependencies of {@link createRoomActions}. */
export interface RoomActionsDeps {
  registry: RoomActionsRegistry;
  collapsed: RoomActionsCollapsed;
  /** Resolves the ids of the windows belonging to a room. */
  windowIdsOf: (roomId: string) => string[];
  /** Measures the area the tiled windows fill; `null` when it cannot be measured. */
  measureArea: () => Size | null;
}

/** The operations a room heading offers, each taking the room it acts on. */
export interface RoomActions {
  /** Collapses or expands a room. */
  toggleCollapsed: (roomId: string) => void;
  /** Tiles a room's windows over the workspace, minimizing the other rooms'. */
  tile: (roomId: string) => void;
  /** Minimizes every window of a room. */
  minimizeAll: (roomId: string) => void;
  /** Restores every window of a room. */
  restoreAll: (roomId: string) => void;
}

/**
 * Creates the per-room operations over the injected dependencies.
 *
 * @param deps the registry, collapsed-room store and readers to act through.
 */
export function createRoomActions(deps: RoomActionsDeps): RoomActions {
  return {
    toggleCollapsed: (roomId) => {
      deps.collapsed.toggle(roomId);
    },
    tile: (roomId) => {
      const windowIds = deps.windowIdsOf(roomId);
      if (windowIds.length === 0) {
        return;
      }
      const area = deps.measureArea();
      if (area === null) {
        return;
      }
      deps.registry.arrangeWindowsOf(area, windowIds);
    },
    minimizeAll: (roomId) => {
      deps.registry.setWindowsMinimized(deps.windowIdsOf(roomId), true);
    },
    restoreAll: (roomId) => {
      deps.registry.setWindowsMinimized(deps.windowIdsOf(roomId), false);
    },
  };
}
