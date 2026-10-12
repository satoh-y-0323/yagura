import { createGuardedLocalStorage } from '../storage/guarded-storage';

/** Narrowest the room rail may become on a wide enough panel, in CSS pixels. */
export const ROOM_RAIL_WIDTH_MIN = 120;

/** Width the room rail starts at when nothing usable is stored, in CSS pixels. */
export const ROOM_RAIL_WIDTH_DEFAULT = 160;

/** Key the room rail width is persisted under. */
const STORAGE_KEY = 'yagura.chat-room-rail-width';

/** A non-finite width falls to the default; anything below the minimum is raised to it. */
function atLeastMinimum(px: number): number {
  if (!Number.isFinite(px)) {
    return ROOM_RAIL_WIDTH_DEFAULT;
  }
  return Math.max(px, ROOM_RAIL_WIDTH_MIN);
}

/**
 * Brings a width into `[ROOM_RAIL_WIDTH_MIN, floor(panelWidth / 2)]`.
 *
 * On a panel narrower than twice the minimum the half-panel maximum wins, so
 * the rail never takes more than half the panel. A non-finite width carries no
 * position to round towards, so {@link ROOM_RAIL_WIDTH_DEFAULT} goes through
 * the same range instead. Never throws.
 */
export function clampRoomRailWidth(px: number, panelWidth: number): number {
  return Math.min(atLeastMinimum(px), Math.floor(panelWidth / 2));
}

/**
 * Reads a persisted width back. Absent, blank, and non-numeric values give
 * {@link ROOM_RAIL_WIDTH_DEFAULT}; a number below the minimum is raised to it.
 * There is no upper bound here because it depends on the panel, which
 * {@link clampRoomRailWidth} applies when drawing. Never throws.
 */
export function readStoredRoomRailWidth(raw: string | null): number {
  if (raw === null || raw.trim() === '') {
    return ROOM_RAIL_WIDTH_DEFAULT;
  }
  return atLeastMinimum(Number(raw));
}

/** The room rail width as it survives a restart, never below the minimum. */
export interface RoomRailWidthStorage {
  /** The persisted width, or the default when none is readable. */
  load(): number;
  /** Persists a width, raised to the minimum. Best effort, like the storage under it. */
  save(px: number): void;
}

/**
 * Builds a {@link RoomRailWidthStorage} over `localStorage`. Storage failures
 * degrade in `createGuardedLocalStorage`, which is the one place that guard is
 * written.
 *
 * @param override storage to use instead of `globalThis.localStorage`, for tests and embedders.
 */
export function createRoomRailWidthStorage(
  override?: Pick<Storage, 'getItem' | 'setItem'>
): RoomRailWidthStorage {
  const storage = createGuardedLocalStorage(STORAGE_KEY, override);

  return {
    load: () => readStoredRoomRailWidth(storage.read()),
    save: (px) => {
      storage.write(String(atLeastMinimum(px)));
    },
  };
}
