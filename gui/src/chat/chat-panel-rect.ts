/** Where the chat panel sits and how big it is, in viewport pixels. */
export interface ChatPanelRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** A width and a height, in CSS pixels. */
interface Size {
  width: number;
  height: number;
}

/** Smallest the chat panel may become while the window has room for it. */
export const CHAT_PANEL_MIN_SIZE: Size = { width: 420, height: 320 };

/** Size the chat panel opens at when nothing usable is stored. */
export const CHAT_PANEL_DEFAULT_SIZE: Size = { width: 560, height: 520 };

const PANEL_MARGIN = 24;

/**
 * The rect to show for `candidate` in `viewport`: at least
 * {@link CHAT_PANEL_MIN_SIZE}, never larger than the window (the window wins
 * when it is smaller than the minimum), and moved back inside it. Returns
 * `candidate` itself when it already fits.
 */
export function clampChatPanelRect(candidate: ChatPanelRect, viewport: Size): ChatPanelRect {
  const width = Math.min(Math.max(candidate.width, CHAT_PANEL_MIN_SIZE.width), viewport.width);
  const height = Math.min(Math.max(candidate.height, CHAT_PANEL_MIN_SIZE.height), viewport.height);
  const x = Math.min(Math.max(candidate.x, 0), Math.max(viewport.width - width, 0));
  const y = Math.min(Math.max(candidate.y, 0), Math.max(viewport.height - height, 0));
  if (
    x === candidate.x &&
    y === candidate.y &&
    width === candidate.width &&
    height === candidate.height
  ) {
    return candidate;
  }
  return { x, y, width, height };
}

/** The room from where `rect` sits to the far edges of `viewport`. */
export function chatPanelMaxSize(rect: ChatPanelRect, viewport: Size): Size {
  return { width: viewport.width - rect.x, height: viewport.height - rect.y };
}

/** Where the panel opens when nothing is stored: the default size, kept to the right edge. */
export function defaultChatPanelRect(viewport: Size): ChatPanelRect {
  return {
    x: viewport.width - CHAT_PANEL_DEFAULT_SIZE.width - PANEL_MARGIN,
    y: PANEL_MARGIN * 3,
    ...CHAT_PANEL_DEFAULT_SIZE,
  };
}

/**
 * Reads a persisted rect back as the rect the user chose. Only a JSON object
 * with finite numbers for `x`, `y`, `width` and `height` is trusted; anything
 * else gives `null`. A size below {@link CHAT_PANEL_MIN_SIZE} is raised to it;
 * the window is not consulted, fitting is left to {@link clampChatPanelRect}.
 * Never throws.
 */
export function readStoredChatPanelRect(raw: string | null): ChatPanelRect | null {
  if (raw === null) {
    return null;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    // A value written by another build, or by something else entirely.
    return null;
  }
  if (typeof parsed !== 'object' || parsed === null) {
    return null;
  }
  const record = parsed as Record<string, unknown>;
  const values = [record['x'], record['y'], record['width'], record['height']];
  if (!values.every((value) => typeof value === 'number' && Number.isFinite(value))) {
    return null;
  }
  const [x, y, width, height] = values as [number, number, number, number];
  return {
    x,
    y,
    width: Math.max(width, CHAT_PANEL_MIN_SIZE.width),
    height: Math.max(height, CHAT_PANEL_MIN_SIZE.height),
  };
}
