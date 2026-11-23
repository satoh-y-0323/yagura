/**
 * Display preparation for the one-line parts of a ticket. A ticket is written
 * by an AI and read by a person; its text of several lines goes through
 * `formatTicketText` (`untrusted-text.ts`).
 */
import type { TicketSummary, TicketWorkspace } from '../ipc/ticket-boundary';
import { formatPathForDisplay, sanitizeForDisplay } from './untrusted-text';

/** The one-line parts of a ticket summary, ready to draw as text. */
export interface TicketSummaryDisplay {
  /** `T-<n>`. */
  label: string;
  title: string;
  state: string;
  milestone: string | null;
  updated_at: number;
}

/**
 * The one-line parts of a ticket summary, each through `sanitizeForDisplay`.
 * @param summary the summary to display
 */
export function displayTicketSummary(summary: TicketSummary): TicketSummaryDisplay {
  return {
    label: `T-${summary.number}`,
    title: sanitizeForDisplay(summary.title),
    state: sanitizeForDisplay(summary.state),
    milestone: summary.milestone === null ? null : sanitizeForDisplay(summary.milestone),
    updated_at: summary.updated_at,
  };
}

/**
 * A workspace row with its kind through `sanitizeForDisplay` and its path
 * through `formatPathForDisplay`, so a path with hidden characters can pass
 * neither for another repository nor for a reordered one.
 * @param workspace the workspace to display
 */
export function displayTicketWorkspace(workspace: TicketWorkspace): TicketWorkspace {
  return {
    workspace_id: workspace.workspace_id,
    kind: sanitizeForDisplay(workspace.kind),
    path: formatPathForDisplay(workspace.path),
  };
}
