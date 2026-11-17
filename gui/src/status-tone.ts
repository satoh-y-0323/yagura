import type { DetectedStatusView, WindowStatus } from './ipc/session-boundary';
import { STATUS_PRESENTATION } from './status-presentation';

/**
 * The entries that need the user, in input order. The single place every
 * "needs you" display location reads from, so none of them decide
 * membership independently; which statuses count comes from the
 * presentation table's `needsYou` column.
 *
 * Written against a minimal structural shape rather than the full
 * `WindowEntry` type, so this module stays independent of the store.
 */
export function windowsNeedingYou<T extends { detected: DetectedStatusView }>(
  entries: readonly T[]
): T[] {
  return entries.filter((entry) => STATUS_PRESENTATION[entry.detected.status].needsYou);
}

/**
 * Counts the entries that need the user's attention — exactly the size of
 * {@link windowsNeedingYou}'s result. Shared by every "needs you" display
 * location so none of them count independently.
 */
export function attentionCount<T extends { detected: DetectedStatusView }>(
  entries: readonly T[]
): number {
  return windowsNeedingYou(entries).length;
}

/** What a group of windows shows as one mark (a room heading, the Rail). */
export interface WindowStatusSummary {
  /** Windows that need the user. */
  count: number;
  /** The highest-ranked status in the group, `null` for an empty group. */
  mark: WindowStatus | null;
  /** Nothing needs the user, but a window is unknown: show the unknown mark alone, no count. */
  unknownOnly: boolean;
}

/** Gathers a group of windows into the one mark and count a collapsed display shows. */
export function summarizeWindowStatuses<T extends { detected: DetectedStatusView }>(
  entries: readonly T[]
): WindowStatusSummary {
  let mark: WindowStatus | null = null;
  let hasUnknown = false;
  for (const { detected } of entries) {
    if (
      mark === null ||
      STATUS_PRESENTATION[detected.status].rank < STATUS_PRESENTATION[mark].rank
    ) {
      mark = detected.status;
    }
    hasUnknown ||= detected.status === 'unknown';
  }
  const count = attentionCount(entries);
  return { count, mark, unknownOnly: count === 0 && hasUnknown };
}
