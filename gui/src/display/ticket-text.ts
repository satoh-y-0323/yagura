/**
 * Display preparation for ticket text. A ticket is written by an AI and read
 * by a person, so every part of it is drawn through these functions.
 */
import type { TicketSummary, TicketWorkspace } from '../ipc/ticket-boundary';
import { formatMultilineText, sanitizeForDisplay } from './untrusted-text';

/**
 * Longest ticket body or result kept for display, in code points. The daemon
 * caps a body at 16 KiB of UTF-8, which is never more code points than this.
 */
export const MAX_TICKET_TEXT_CHARS = 16 * 1024;

/** The one-line parts of a ticket summary, ready to draw as text. */
export interface TicketSummaryDisplay {
  /** `T-<n>`. */
  label: string;
  title: string;
  state: string;
  milestone: string | null;
  updated_at: number;
}

const TICKET_TEXT_KEPT_CONTROLS: ReadonlySet<string> = new Set(['\n', '\t']);

/**
 * Prepares a ticket's body, a result, or other text of several lines for
 * display. Escaped like an approval body, except that tabs are kept and the
 * cap is {@link MAX_TICKET_TEXT_CHARS}.
 * @param value the text to display
 */
export function formatTicketText(value: string): string {
  return formatMultilineText(value, MAX_TICKET_TEXT_CHARS, TICKET_TEXT_KEPT_CONTROLS);
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

const NO_KEPT_CONTROLS: ReadonlySet<string> = new Set();

/**
 * A workspace row with its kind and path through `sanitizeForDisplay`. The
 * path also has its bidirectional controls written out, as an approval body
 * does, so a reordered path cannot pass for another repository.
 * @param workspace the workspace to display
 */
export function displayTicketWorkspace(workspace: TicketWorkspace): TicketWorkspace {
  return {
    workspace_id: workspace.workspace_id,
    kind: sanitizeForDisplay(workspace.kind),
    // Already capped by `sanitizeForDisplay`, so it is not cut a second time.
    path: formatMultilineText(
      sanitizeForDisplay(workspace.path),
      Number.POSITIVE_INFINITY,
      NO_KEPT_CONTROLS
    ),
  };
}
