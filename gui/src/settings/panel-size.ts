import { createGuardedLocalStorage } from '../storage/guarded-storage';

/** A width and a height, in CSS pixels. */
export interface PanelSize {
  width: number;
  height: number;
}

/** Narrowest a resizable floating panel (Settings, Plans) may become. */
export const PANEL_MIN_SIZE: PanelSize = { width: 360, height: 280 };

/** Size a panel starts at when nothing usable is stored. */
export const PANEL_DEFAULT_SIZE: PanelSize = { width: 640, height: 520 };

/** A floating panel whose size this module persists. Each keeps its own key. */
export type PanelKind = 'settings' | 'plans';

/** Persistence key for one panel kind, apart from the others and from the sidebar's own storage. */
function storageKey(panel: PanelKind): string {
  return `yagura.${panel}-panel-size`;
}

/**
 * Margin reserved between the panel and the edge of the window on each
 * axis, so a maximized panel still shows the window around it (the same
 * margin the panel's own fixed position used before it became resizable).
 */
const VIEWPORT_MARGIN = 96;

/**
 * The largest one axis may be: `viewportAxis - VIEWPORT_MARGIN`, but never
 * below what the window can show of `min` -- when the window is narrower
 * than that, the window wins even if it is less than `min`.
 */
function upperAxis(viewportAxis: number, min: number): number {
  return Math.max(Math.min(min, viewportAxis), viewportAxis - VIEWPORT_MARGIN);
}

/**
 * The largest size `viewport` allows a panel: the bound {@link clampPanelSize}
 * rounds to, and the maximum a panel is drawn with, so that nothing outside
 * the clamp -- the native corner included -- can draw it larger.
 */
export function panelMaxSize(viewport: PanelSize): PanelSize {
  return {
    width: upperAxis(viewport.width, PANEL_MIN_SIZE.width),
    height: upperAxis(viewport.height, PANEL_MIN_SIZE.height),
  };
}

/** Brings one axis into `[min, upper]`, falling back to `defaultValue` for a non-finite input. */
function clampAxis(value: number, upper: number, min: number, defaultValue: number): number {
  const base = Number.isFinite(value) ? value : defaultValue;
  return Math.min(Math.max(base, min), upper);
}

/**
 * Brings `size` into range for `viewport`: at least {@link PANEL_MIN_SIZE}
 * on each axis, but never past what `viewport` allows. A non-finite axis
 * falls to {@link PANEL_DEFAULT_SIZE}'s value for that axis. Never throws.
 */
export function clampPanelSize(size: PanelSize, viewport: PanelSize): PanelSize {
  const max = panelMaxSize(viewport);
  return {
    width: clampAxis(size.width, max.width, PANEL_MIN_SIZE.width, PANEL_DEFAULT_SIZE.width),
    height: clampAxis(size.height, max.height, PANEL_MIN_SIZE.height, PANEL_DEFAULT_SIZE.height),
  };
}

/** Differences below this are bounding-box fractions, not a drag. */
const RESIZE_TOLERANCE = 0.5;

/**
 * Whether a size read back from a panel's native corner differs from the size
 * the panel is shown at, i.e. the user dragged the corner. A window resize
 * changes only the size shown, so its read-back matches and is not saved.
 */
export function isResizedByUser(read: PanelSize, shown: PanelSize): boolean {
  return (
    Math.abs(read.width - shown.width) >= RESIZE_TOLERANCE ||
    Math.abs(read.height - shown.height) >= RESIZE_TOLERANCE
  );
}

/** Raises one stored axis to `min`, or falls to `defaultValue` when it is not finite. */
function storedAxis(value: number, min: number, defaultValue: number): number {
  return Number.isFinite(value) ? Math.max(value, min) : defaultValue;
}

/**
 * Reads a persisted panel size back as the size the user chose. Only a JSON
 * object of the exact shape `{"width": number, "height": number}` is trusted;
 * anything else (absent, blank, malformed JSON, a JSON value of another shape,
 * or a field of the wrong type) falls to {@link PANEL_DEFAULT_SIZE}. A
 * non-finite axis falls to the default's value and an axis below
 * {@link PANEL_MIN_SIZE} is raised to it. The window is not consulted: fitting
 * into it is left to {@link clampPanelSize} on the size shown. Never throws.
 */
export function readStoredPanelSize(raw: string | null): PanelSize {
  if (raw === null) {
    return { ...PANEL_DEFAULT_SIZE };
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return { ...PANEL_DEFAULT_SIZE };
  }

  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    return { ...PANEL_DEFAULT_SIZE };
  }

  const { width, height } = parsed as Record<string, unknown>;
  if (typeof width !== 'number' || typeof height !== 'number') {
    return { ...PANEL_DEFAULT_SIZE };
  }

  return {
    width: storedAxis(width, PANEL_MIN_SIZE.width, PANEL_DEFAULT_SIZE.width),
    height: storedAxis(height, PANEL_MIN_SIZE.height, PANEL_DEFAULT_SIZE.height),
  };
}

/** A panel's chosen size as it survives a reopen. */
export interface PanelSizeStorage {
  /** The persisted chosen size, or the default when none is readable. */
  load(): PanelSize;
  /** Persists the chosen size verbatim. */
  save(size: PanelSize): void;
}

/**
 * Builds a {@link PanelSizeStorage} over `localStorage`, keyed by `panel` so
 * the Settings panel and the Plans panel each remember a size of their own.
 *
 * @param override storage to use instead of `globalThis.localStorage`, for tests and embedders.
 */
export function createPanelSizeStorage(
  panel: PanelKind,
  override?: Pick<Storage, 'getItem' | 'setItem'>
): PanelSizeStorage {
  const storage = createGuardedLocalStorage(storageKey(panel), override);

  return {
    load: () => readStoredPanelSize(storage.read()),
    save: (size) => {
      storage.write(JSON.stringify(size));
    },
  };
}
