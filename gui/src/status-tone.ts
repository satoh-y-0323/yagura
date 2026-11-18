import type { DetectedStatusView, TaskReportMark, WindowStatus } from './ipc/session-boundary';
import { STATUS_PRESENTATION } from './status-presentation';

/**
 * What deciding "needs you" reads from an entry. A minimal structural shape
 * rather than the full `WindowEntry` type, so this module stays independent
 * of the store.
 */
export interface NeedsYouInput {
  detected: DetectedStatusView;
  taskReport?: TaskReportMark;
}

/**
 * Whether one entry needs the user: the single decision every "needs you"
 * display location reads. Which statuses count comes from the presentation
 * table's `needsYou` column; a task window waiting for input while its report
 * still stands is waiting on its owner, not on the user.
 */
export function needsYou(entry: NeedsYouInput): boolean {
  const { status } = entry.detected;
  if (status === 'waiting_input' && entry.taskReport?.standing === true) {
    return false;
  }
  return STATUS_PRESENTATION[status].needsYou;
}

/** The entries that need the user ({@link needsYou}), in input order. */
export function windowsNeedingYou<T extends NeedsYouInput>(entries: readonly T[]): T[] {
  return entries.filter(needsYou);
}

/**
 * Counts the entries that need the user's attention — exactly the size of
 * {@link windowsNeedingYou}'s result. Shared by every "needs you" display
 * location so none of them count independently.
 */
export function attentionCount<T extends NeedsYouInput>(entries: readonly T[]): number {
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
export function summarizeWindowStatuses<T extends NeedsYouInput>(
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
