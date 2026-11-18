/**
 * The "Needs you" filter's row set. The set is rebuilt only when the user
 * operates; between operations a row that stops needing the user stays (as
 * settled) and a window that starts needing the user is appended, so no row
 * moves without the user asking.
 */
import { needsYou, type NeedsYouInput } from '../status-tone';
import { windowsInDisplayOrder, type RoomGroup } from './room-groups';

/** One window listed in the filter. */
export interface NeedsYouRow {
  windowId: string;
  /** The window no longer needs the user; the row stays until the next rebuild. */
  settled: boolean;
}

/** What the filter shows. */
export interface NeedsYouView {
  /** Windows that need the user (or did when the set was last rebuilt), in display order. */
  rows: NeedsYouRow[];
  /** Windows whose status is unknown: listed under their own heading, never counted as needing the user. */
  unknown: { windowId: string }[];
  /** Neither a row nor an unknown window exists. */
  showsNothingNeedsYou: boolean;
}

type Member = NeedsYouInput & { windowId: string };

/**
 * Computes the filter's next view.
 *
 * @param previous the view last shown, or `null` when there is none.
 * @param groups the sidebar's rooms; the order of the rows follows them.
 * @param userOperated whether the user just asked for a rebuild (turned the filter on, jumped
 * to the next attention window, or picked a row). Only then are settled rows dropped.
 */
export function nextNeedsYouView<T extends Member>(
  previous: NeedsYouView | null,
  groups: readonly RoomGroup<T>[],
  userOperated: boolean
): NeedsYouView {
  const entries = windowsInDisplayOrder(groups);
  const byId = new Map(entries.map((entry) => [entry.windowId, entry]));
  const needing = entries.filter(needsYou);

  let rows: NeedsYouRow[];
  if (previous === null || userOperated) {
    rows = needing.map((entry) => ({ windowId: entry.windowId, settled: false }));
  } else {
    // A closed window has nothing to open, so its row goes even between operations.
    const kept = previous.rows.flatMap((row) => {
      const entry = byId.get(row.windowId);
      return entry === undefined ? [] : [{ windowId: row.windowId, settled: !needsYou(entry) }];
    });
    const known = new Set(kept.map((row) => row.windowId));
    const joined = needing
      .filter((entry) => !known.has(entry.windowId))
      .map((entry) => ({ windowId: entry.windowId, settled: false }));
    rows = [...kept, ...joined];
  }

  // A kept row whose window turned unknown stays a row, so one window is never listed twice.
  const listed = new Set(rows.map((row) => row.windowId));
  const unknown = entries
    .filter((entry) => entry.detected.status === 'unknown' && !listed.has(entry.windowId))
    .map((entry) => ({ windowId: entry.windowId }));

  return { rows, unknown, showsNothingNeedsYou: rows.length === 0 && unknown.length === 0 };
}
