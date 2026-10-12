import { createGuardedLocalStorage } from '../storage/guarded-storage';

/** Narrowest the sidebar may become, in CSS pixels. */
export const SIDEBAR_WIDTH_MIN = 200;

/** Widest the sidebar may become, in CSS pixels. */
export const SIDEBAR_WIDTH_MAX = 560;

/** Width the sidebar starts at when nothing usable is stored, in CSS pixels. */
export const SIDEBAR_WIDTH_DEFAULT = 288;

/** Key the sidebar width is persisted under. */
const STORAGE_KEY = 'yagura.sidebar-width';

/**
 * Brings a width into `[SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX]`.
 *
 * A non-finite width carries no position to round towards, so it falls to
 * {@link SIDEBAR_WIDTH_DEFAULT}. Never throws.
 */
export function clampSidebarWidth(px: number): number {
  if (!Number.isFinite(px)) {
    return SIDEBAR_WIDTH_DEFAULT;
  }
  return Math.min(Math.max(px, SIDEBAR_WIDTH_MIN), SIDEBAR_WIDTH_MAX);
}

/**
 * Reads a persisted width back. Absent, blank, and non-numeric values give
 * {@link SIDEBAR_WIDTH_DEFAULT}; an out-of-range number is rounded by
 * {@link clampSidebarWidth}. Never throws, so a corrupted value cannot keep
 * the sidebar from being drawn.
 */
export function readStoredSidebarWidth(raw: string | null): number {
  if (raw === null || raw.trim() === '') {
    return SIDEBAR_WIDTH_DEFAULT;
  }
  return clampSidebarWidth(Number(raw));
}

/** The sidebar width as it survives a restart, always within the declared range. */
export interface SidebarWidthStorage {
  /** The persisted width, or the default when none is readable. */
  load(): number;
  /** Persists a width, rounded into range. Best effort, like the storage under it. */
  save(px: number): void;
}

/**
 * Builds a {@link SidebarWidthStorage} over `localStorage`.
 *
 * The width is how one machine shows the application, not part of the restored
 * session, so it lives here rather than in the session store. Storage failures
 * degrade in `createGuardedLocalStorage`, which is the one place that guard is
 * written.
 *
 * @param override storage to use instead of `globalThis.localStorage`, for tests and embedders.
 */
export function createSidebarWidthStorage(
  override?: Pick<Storage, 'getItem' | 'setItem'>
): SidebarWidthStorage {
  const storage = createGuardedLocalStorage(STORAGE_KEY, override);

  return {
    load: () => readStoredSidebarWidth(storage.read()),
    save: (px) => {
      storage.write(String(clampSidebarWidth(px)));
    },
  };
}
