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
  /** The window's room as the sidebar keys it ({@link RoomGroup.roomId}). */
  roomId: string;
  /** The room's display name ({@link RoomGroup.name}), never the raw value. */
  roomName: string;
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

/** A run of consecutive rows of one room, drawn under one heading. */
export interface NeedsYouSection {
  roomId: string;
  name: string;
  rows: NeedsYouRow[];
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
  const roomOf = new Map(
    groups.flatMap((group) => group.entries.map((entry) => [entry.windowId, group] as const))
  );
  const needing = entries.filter(needsYou);
  // The room is read afresh on every pass, so a kept row follows its window into another room.
  const rowOf = (entry: T, settled: boolean): NeedsYouRow[] => {
    const group = roomOf.get(entry.windowId);
    return group === undefined
      ? []
      : [{ windowId: entry.windowId, settled, roomId: group.roomId, roomName: group.name }];
  };

  let rows: NeedsYouRow[];
  if (previous === null || userOperated) {
    rows = needing.flatMap((entry) => rowOf(entry, false));
  } else {
    // A closed window has nothing to open, so its row goes even between operations.
    const kept = previous.rows.flatMap((row) => {
      const entry = byId.get(row.windowId);
      return entry === undefined ? [] : rowOf(entry, !needsYou(entry));
    });
    const known = new Set(kept.map((row) => row.windowId));
    const joined = needing
      .filter((entry) => !known.has(entry.windowId))
      .flatMap((entry) => rowOf(entry, false));
    rows = [...kept, ...joined];
  }

  // A kept row whose window turned unknown stays a row, so one window is never listed twice.
  const listed = new Set(rows.map((row) => row.windowId));
  const unknown = entries
    .filter((entry) => entry.detected.status === 'unknown' && !listed.has(entry.windowId))
    .map((entry) => ({ windowId: entry.windowId }));

  return { rows, unknown, showsNothingNeedsYou: rows.length === 0 && unknown.length === 0 };
}

/**
 * The windows in the order the sidebar shows them, which the next and previous
 * window keys walk: the sidebar order while the filter is off, otherwise the
 * filter's rows (settled ones included) followed by its unknown windows.
 *
 * @param groups the sidebar's rooms; collapsed rooms' windows are included.
 * @param view the filter's view, or `null` while the filter is off.
 */
export function visibleWindowOrder<T extends { windowId: string }>(
  groups: readonly RoomGroup<T>[],
  view: NeedsYouView | null
): T[] {
  const entries = windowsInDisplayOrder(groups);
  if (view === null) {
    return entries;
  }
  const byId = new Map(entries.map((entry) => [entry.windowId, entry]));
  return [...view.rows, ...view.unknown].flatMap((row) => byId.get(row.windowId) ?? []);
}

/**
 * Splits the filter's rows into headed runs: a new section starts wherever the
 * room changes, so the rows keep their order and a room whose rows are not
 * adjacent gets its heading again.
 *
 * @param rows the filter's rows, in the order they are shown.
 */
export function needsYouSections(rows: readonly NeedsYouRow[]): NeedsYouSection[] {
  const sections: NeedsYouSection[] = [];
  for (const row of rows) {
    const last = sections[sections.length - 1];
    if (last !== undefined && last.roomId === row.roomId) {
      last.rows.push(row);
    } else {
      sections.push({ roomId: row.roomId, name: row.roomName, rows: [row] });
    }
  }
  return sections;
}
