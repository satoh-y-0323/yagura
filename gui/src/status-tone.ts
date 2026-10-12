import type { DetectedStatusView, WindowStatus } from './ipc/session-boundary';

/**
 * The display tone a window status resolves to. Every display location
 * reads this instead of re-deriving its own classification, so the same status
 * always looks the same everywhere.
 */
export type StatusTone = 'attention' | 'active' | 'halted' | 'quiet' | 'unknown';

/**
 * Total map over the {@link WindowStatus} values. Written as a
 * `Record`, not a `switch` with a `default` branch, so adding a status value
 * fails to compile here instead of silently falling through.
 */
const TONE_BY_STATUS: Record<WindowStatus, StatusTone> = {
  waiting_input: 'attention',
  waiting_approval: 'attention',
  running: 'active',
  interrupted: 'halted',
  idle: 'quiet',
  exited: 'quiet',
  unknown: 'unknown',
};

/** Resolves a window status into its display tone. */
export function resolveTone(status: WindowStatus): StatusTone {
  return TONE_BY_STATUS[status];
}

/**
 * The entries whose tone is `attention`, in input order. The single place
 * every "needs you" display location reads from, so none of them decide
 * membership independently.
 *
 * Written against a minimal structural shape rather than the full
 * `WindowEntry` type, so this module stays independent of the store.
 */
export function windowsNeedingYou<T extends { detected: DetectedStatusView }>(
  entries: readonly T[]
): T[] {
  return entries.filter((entry) => resolveTone(entry.detected.status) === 'attention');
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
